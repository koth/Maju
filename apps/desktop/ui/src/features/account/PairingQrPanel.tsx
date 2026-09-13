import { useCallback, useEffect, useRef, useState } from "react";
import QRCode from "qrcode";
import {
  remoteControlPairingQr,
  remoteControlStatus,
} from "../../lib/tauri";

interface Props {
  refreshKey: number;
}

type Status = "idle" | "minting" | "ready" | "error";

/** Re-mint this long before the code's TTL runs out (the relay TTL is 120s).
 * Letting the shown QR expire silently is a guaranteed "scanned and nothing
 * happened" report: the phone reads the code, the relay rejects it. */
const REFRESH_BEFORE_EXPIRY_SECS = 30;
/** Hard floor between automatic re-mints. Without it, a host build that
 * reports no expiry would see `expiresIn == null` on every 3s poll and re-mint
 * forever — and each mint makes the PC driver reconnect to re-register. */
const MIN_REMINT_INTERVAL_MS = 20_000;

/**
 * Pairing QR for the mobile companion.
 *
 * The QR must only be presented as scannable when it actually is:
 *  - the relay link is up (a code minted offline is never registered), and
 *  - the minted code has been acked by the relay, and
 *  - the code is still inside its TTL.
 *
 * These used to be invisible: a QR was drawn unconditionally while the panel
 * below it said "○ 未连接 relay", and the code could silently expire after
 * 120s. Scanning either shape fails with a relay-side "invalid or expired
 * pairing code", which reads as "the phone does nothing". Each state now has
 * its own explicit message, and the panel re-mints before expiry.
 */
export function PairingQrPanel({ refreshKey }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [status, setStatus] = useState<Status>("idle");
  const [error, setError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const [registered, setRegistered] = useState(false);
  const [expiresIn, setExpiresIn] = useState<number | null>(null);
  const lastMintAtRef = useRef(0);

  const mint = useCallback(async () => {
    lastMintAtRef.current = Date.now();
    setStatus("minting");
    setError(null);
    try {
      const json = await remoteControlPairingQr();
      if (!json) {
        setStatus("error");
        setError("远程控制已禁用");
        return;
      }
      if (canvasRef.current) {
        await QRCode.toCanvas(canvasRef.current, json, {
          width: 220,
          margin: 2,
          errorCorrectionLevel: "M",
          color: { dark: "#0b0c0e", light: "#ffffff" },
        });
      }
      setStatus("ready");
    } catch (e) {
      setStatus("error");
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void mint();
  }, [mint, refreshKey]);

  // Poll the backend status: relay reachability, whether this code is
  // registered, and how long it stays valid.
  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const s = await remoteControlStatus();
        if (!active) return;
        setConnected(s.connected);
        setRegistered(s.pairing_registered);
        setExpiresIn(s.pairing_expires_in_secs ?? null);
      } catch {
        // best-effort; keep last known state
      }
    };
    void poll();
    const id = window.setInterval(poll, 3000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, []);

  // Re-mint before the code expires. Deliberately keyed on the BACKEND's
  // remaining seconds (authoritative wall clock) rather than a local timer
  // started at mint time, which drifts across sleeps/resumes. Null means "no
  // live code" (expired and cleared by the backend), which also needs a mint.
  useEffect(() => {
    if (status === "minting") return;
    if (!connected) return;
    const stale = expiresIn == null || expiresIn <= REFRESH_BEFORE_EXPIRY_SECS;
    if (!stale) return;
    if (Date.now() - lastMintAtRef.current < MIN_REMINT_INTERVAL_MS) return;
    void mint();
  }, [connected, expiresIn, status, mint]);

  const usable = connected && registered;
  const waitingForRelay = !connected;
  const waitingForRegistration = connected && !registered;

  return (
    <div className="account-pairing">
      <span className="account-hint">配对二维码</span>
      <div className="account-pairing-body">
        <canvas
          ref={canvasRef}
          className={`account-qr-canvas${usable ? "" : " is-unusable"}`}
          aria-label="PC 配对二维码"
          role="img"
        />
        <div className="account-pairing-meta">
          <p className="account-pairing-step">
            1. 在手机 Maju app 打开「配对」，扫描上方二维码。
          </p>
          <p className="account-pairing-step">
            2. 配对成功后 PC 会自动保持连接，手机可远程控制。
          </p>
          <p
            className={`account-pairing-conn ${connected ? "is-on" : "is-off"}`}
          >
            {connected ? "● 已连接 relay" : "○ 未连接 relay"}
          </p>

          {waitingForRelay && (
            <p className="account-pairing-warning">
              电脑还没连上 relay，此时扫码手机一定配不上。请检查电脑网络后点「手动刷新」。
            </p>
          )}
          {waitingForRegistration && (
            <p className="account-pairing-warning">
              正在把配对码注册到 relay…注册完成前扫码会失败。
            </p>
          )}
          {usable && expiresIn != null && (
            <p className="account-pairing-ttl">
              二维码有效，剩余 {expiresIn}s（到期自动刷新）
            </p>
          )}
          {usable && expiresIn == null && (
            <p className="account-pairing-ttl">二维码有效，可扫码配对</p>
          )}

          {status === "minting" && <p className="account-pairing-ttl">生成中…</p>}
          {status === "error" && <div className="account-error">{error}</div>}
          <button
            type="button"
            className="account-secondary-btn"
            onClick={() => void mint()}
            disabled={status === "minting"}
          >
            手动刷新
          </button>
        </div>
      </div>
    </div>
  );
}
