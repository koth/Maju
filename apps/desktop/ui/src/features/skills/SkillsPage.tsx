import { useCallback, useEffect, useRef, useState } from "react";
import type { InstalledSkill, SkillHubSkill } from "../../types";
import {
  skillsListInstalled,
  skillsUninstall,
  skillhubInstallCli,
  skillhubInstall,
  skillhubRankings,
  skillhubSearch,
  skillhubStatus,
} from "../../lib/tauri";
import { appConfirm } from "../../lib/confirm";
import { listen } from "@tauri-apps/api/event";
import "./SkillsPage.css";

interface Props {
  onBack: () => void;
  onTabChange?: (tab: Tab) => void;
}

type Tab = "installed" | "skillhub";

interface InstallingState {
  name: string;
  progress: string;
}

const RANKING_TYPES = [
  { value: "hot", label: "热门" },
  { value: "featured", label: "精选" },
  { value: "newest", label: "最新" },
  { value: "recommended", label: "推荐" },
  { value: "trending", label: "趋势" },
] as const;

/** 上架日期（YYYY-MM-DD）。用于肉眼校验「最新」是否真的按时间倒序。 */
function formatPublishDate(createdAt?: number | null): string {
  if (!createdAt) return "";
  const d = new Date(createdAt);
  if (Number.isNaN(d.getTime())) return "";
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
    d.getDate(),
  ).padStart(2, "0")}`;
}

/** 优先展示 namespace.handle（比 ownerName 的用户 id 更可读）。 */
function skillAuthor(skill: SkillHubSkill): string {
  return skill.namespace?.handle || skill.author || "";
}

/** 大数字压缩：1234 → 1.2k */
function formatCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** 分类的中文标签，用于标题与错误文案。 */
function rankingLabel(type: string): string {
  return RANKING_TYPES.find((t) => t.value === type)?.label ?? type;
}

export function SkillsPage({ onBack, onTabChange }: Props) {
  const [tab, setTab] = useState<Tab>("installed");
  const [installed, setInstalled] = useState<InstalledSkill[]>([]);
  const [skillhubResults, setSkillhubResults] = useState<SkillHubSkill[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [installing, setInstalling] = useState<InstallingState | null>(null);
  const [uninstalling, setUninstalling] = useState<string | null>(null);

  // SkillHub state
  const [cliAvailable, setCliAvailable] = useState<boolean | null>(null);
  const [cliInstalling, setCliInstalling] = useState(false);
  const [cliInstallLogs, setCliInstallLogs] = useState<string[]>([]);
  const cliLogRef = useRef<HTMLDivElement>(null);
  const [skillhubQuery, setSkillhubQuery] = useState("");
  const [skillhubSearching, setSkillhubSearching] = useState(false);

  // Rankings state
  const [rankingType, setRankingType] = useState<string>("hot");
  const [rankingLoading, setRankingLoading] = useState(false);
  const [rankingLoaded, setRankingLoaded] = useState(false);
  // Which list the grid is currently showing. Keeping this explicit stops a
  // search from silently masquerading as a ranking (and vice versa).
  const [viewMode, setViewMode] = useState<"ranking" | "search">("ranking");

  const switchTab = useCallback(
    (newTab: Tab) => {
      setTab(newTab);
      onTabChange?.(newTab);
    },
    [onTabChange],
  );

  const loadInstalled = useCallback(async () => {
    try {
      const list = await skillsListInstalled();
      setInstalled(list.filter((s) => !s.is_system));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const checkCliStatus = useCallback(async () => {
    try {
      const available = await skillhubStatus();
      setCliAvailable(available);
    } catch {
      setCliAvailable(false);
    }
  }, []);

  const loadRankings = useCallback(async (type: string) => {
    setRankingLoading(true);
    setError(null);
    try {
      const results = await skillhubRankings(type);
      setSkillhubResults(results);
      setViewMode("ranking");
      setRankingLoaded(true);
    } catch (e) {
      // Drop the previous category's rows so a failed switch cannot masquerade
      // as the newly selected one.
      setSkillhubResults([]);
      setError(`加载${rankingLabel(type)}失败: ${e}`);
    } finally {
      setRankingLoading(false);
    }
  }, []);

  useEffect(() => {
    loadInstalled();
    checkCliStatus();
  }, [loadInstalled, checkCliStatus]);

  // Listen for CLI install progress events
  useEffect(() => {
    const unlisten = listen<string>("skillhub:cli-install-progress", (event) => {
      setCliInstallLogs((prev) => [...prev, event.payload]);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Auto-scroll CLI install logs
  useEffect(() => {
    if (cliLogRef.current) {
      cliLogRef.current.scrollTop = cliLogRef.current.scrollHeight;
    }
  }, [cliInstallLogs]);

  // The API's empty-query search is the platform's default (most popular)
  // listing — a CLI-free way to show something useful.
  const loadDefaultListing = useCallback(async () => {
    setRankingLoading(true);
    setError(null);
    try {
      const results = await skillhubSearch("");
      setSkillhubResults(results);
      setViewMode("ranking");
      setRankingLoaded(true);
    } catch (e) {
      setSkillhubResults([]);
      setError(`加载 SkillHub 列表失败: ${e}`);
    } finally {
      setRankingLoading(false);
    }
  }, []);

  // Entering the SkillHub tab: prefer the CLI ranking board, but fall back to
  // the platform's default listing (empty-query search) when the CLI is absent
  // so the panel is never blank.
  useEffect(() => {
    if (tab !== "skillhub" || rankingLoaded || rankingLoading) return;
    if (cliAvailable === null) return; // still probing
    if (cliAvailable) {
      loadRankings(rankingType);
    } else {
      loadDefaultListing();
    }
  }, [
    tab,
    cliAvailable,
    rankingLoaded,
    rankingLoading,
    rankingType,
    loadRankings,
    loadDefaultListing,
  ]);

  const handleInstallSkillHub = useCallback(
    async (skill: SkillHubSkill) => {
      setInstalling({ name: skill.slug, progress: "正在安装..." });
      setError(null);
      try {
        await skillhubInstall(skill.slug);
        await loadInstalled();
        setInstalling(null);
      } catch (e) {
        setError(`安装失败: ${e}`);
        setInstalling(null);
      }
    },
    [loadInstalled],
  );

  const handleUninstall = useCallback(
    async (skill: InstalledSkill) => {
      const confirmed = await appConfirm({
        title: `确定要删除技能「${skill.name}」吗？`,
        description: "删除后无法恢复。",
      });
      if (!confirmed) return;
      setUninstalling(skill.name);
      setError(null);
      try {
        await skillsUninstall(skill.name);
        await loadInstalled();
      } catch (e) {
        setError(`删除失败: ${e}`);
      } finally {
        setUninstalling(null);
      }
    },
    [loadInstalled],
  );

  const handleInstallCli = useCallback(async () => {
    setCliInstalling(true);
    setCliInstallLogs([]);
    setError(null);
    try {
      await skillhubInstallCli();
      setError(null);
      setCliAvailable(true);
    } catch (e) {
      setError(`CLI 安装失败: ${e}`);
    } finally {
      setCliInstalling(false);
    }
  }, []);

  const handleSkillHubSearch = useCallback(async () => {
    const q = skillhubQuery.trim();
    if (!q) return;
    setSkillhubSearching(true);
    setError(null);
    try {
      const results = await skillhubSearch(q);
      setSkillhubResults(results);
      setViewMode("search");
      // A search replaces the ranking view until a category is picked again.
      setRankingLoaded(true);
    } catch (e) {
      setSkillhubResults([]);
      setError(`搜索「${q}」失败: ${e}`);
    } finally {
      setSkillhubSearching(false);
    }
  }, [skillhubQuery]);

  const handleRankingChange = useCallback(
    (type: string) => {
      if (type === rankingType && viewMode === "ranking" && rankingLoaded) return;
      setRankingType(type);
      setSkillhubResults([]);
      setViewMode("ranking");
      setRankingLoaded(false);
      loadRankings(type);
    },
    [loadRankings, rankingType, rankingLoaded, viewMode],
  );

  const isInstalled = useCallback(
    (name: string) => installed.some((s) => s.name === name),
    [installed],
  );

  return (
    <div className="skills-page">
      <div className="skills-drag-strip" data-tauri-drag-region />
      <header className="skills-header">
        <button type="button" className="skills-back" onClick={onBack}>
          <span className="skills-back-arrow">←</span> 返回应用
        </button>
        <div className="skills-title-row">
          <div>
            <h1>技能</h1>
            <p className="skills-subtitle">
              浏览和安装技能，扩展 Agent 的能力
            </p>
          </div>
        </div>
        <div className="skills-tabs">
          <button
            type="button"
            className={`skills-tab ${tab === "installed" ? "is-active" : ""}`}
            onClick={() => switchTab("installed")}
          >
            已安装
          </button>
          <button
            type="button"
            className={`skills-tab ${tab === "skillhub" ? "is-active" : ""}`}
            onClick={() => switchTab("skillhub")}
          >
            SkillHub
          </button>
        </div>
      </header>

      <div className="skills-content">
        {tab === "installed" && (
          <div className="skills-section">
            <div className="skills-section-header">
              <h2>已安装的技能</h2>
              <span className="skills-count">{installed.length} 个技能</span>
            </div>
            {error && <div className="skills-error">{error}</div>}
            {installed.length === 0 && !error && (
              <div className="skills-empty">
                <span className="skills-empty-title">还没有安装技能</span>
                <span className="skills-empty-copy">
                  切换到「SkillHub」标签页浏览和安装技能
                </span>
              </div>
            )}
            <div className="skills-grid">
              {installed.map((skill) => (
                <div key={skill.name} className="skills-card">
                  <div className="skills-card-head">
                    <div className="skills-card-title">
                      <span className="skills-name">{skill.name}</span>
                    </div>
                    <button
                      type="button"
                      className="skills-action-btn is-danger"
                      onClick={() => handleUninstall(skill)}
                      disabled={uninstalling === skill.name}
                    >
                      {uninstalling === skill.name ? "删除中..." : "删除"}
                    </button>
                  </div>
                  {skill.description && (
                    <div className="skills-card-desc">{skill.description}</div>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        {tab === "skillhub" && (
          <div className="skills-section">
            <div className="skills-section-header">
              <h2>
                {viewMode === "search"
                  ? `「${skillhubQuery.trim()}」的搜索结果`
                  : cliAvailable
                    ? `SkillHub ${rankingLabel(rankingType)}技能`
                    : "搜索 SkillHub 技能"}
              </h2>
              <div className="skills-ranking-tabs">
                {RANKING_TYPES.map((rt) => (
                  <button
                    key={rt.value}
                    type="button"
                    className={`skills-ranking-tab ${rankingType === rt.value && viewMode === "ranking" ? "is-active" : ""}`}
                    onClick={() => handleRankingChange(rt.value)}
                    disabled={rankingLoading || !cliAvailable}
                    title={cliAvailable ? undefined : "分类排行需要 skillhub CLI"}
                  >
                    {rt.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="skills-section-header">
              <div className="skills-repo-inputs">
                <input
                  type="text"
                  className="skills-repo-input"
                  placeholder="搜索技能..."
                  value={skillhubQuery}
                  onChange={(e) => setSkillhubQuery(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                          handleSkillHubSearch();
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="skills-refresh-btn"
                      onClick={handleSkillHubSearch}
                      disabled={skillhubSearching}
                    >
                      {skillhubSearching ? "搜索中..." : "搜索"}
                    </button>
                  </div>
                </div>

                {error && <div className="skills-error">{error}</div>}

                {!cliAvailable && (
                  <div className="skills-cli-hint">
                    <span>
                      搜索和安装可直接使用。热门 / 精选 / 最新等分类排行由 SkillHub 官方
                      CLI 提供，需要额外安装。
                    </span>
                    <button
                      type="button"
                      className="skills-action-btn"
                      onClick={handleInstallCli}
                      disabled={cliInstalling}
                    >
                      {cliInstalling ? "正在安装..." : "安装 CLI 以启用分类"}
                    </button>
                    {cliInstallLogs.length > 0 && (
                      <div className="skills-cli-log" ref={cliLogRef}>
                        {cliInstallLogs.map((line, i) => (
                          <div key={i} className="skills-cli-log-line">
                            {line}
                          </div>
                        ))}
                        {cliInstalling && <div className="skills-cli-log-cursor">▋</div>}
                      </div>
                    )}
                  </div>
                )}

                {(rankingLoading || skillhubSearching) && skillhubResults.length === 0 && (
                  <div className="skills-empty" role="status">
                    {skillhubSearching ? "正在搜索..." : `正在加载${rankingLabel(rankingType)}技能...`}
                  </div>
                )}

                {!rankingLoading && !skillhubSearching && skillhubResults.length === 0 && !error && (
                  <div className="skills-empty">
                    <span className="skills-empty-title">
                      {viewMode === "search" ? "没有匹配的技能" : "暂无技能"}
                    </span>
                    <span className="skills-empty-copy">
                      {viewMode === "search"
                        ? "换个关键词试试"
                        : cliAvailable
                          ? "切换到其他分类或搜索技能"
                          : "用上方搜索框查找并安装技能"}
                    </span>
                  </div>
                )}

                {skillhubResults.length > 0 && (
                  <div className="skills-count-row">
                    共 {skillhubResults.length} 个
                    {viewMode === "search" && "（搜索结果，排名靠后可能相关性较弱）"}
                  </div>
                )}

                <div className="skills-grid">
                  {skillhubResults.map((skill) => {
                    const installed_ = isInstalled(skill.slug);
                    const isInstalling = installing?.name === skill.slug;
                    return (
                      <div key={skill.slug} className="skills-card">
                        <div className="skills-card-head">
                          <div className="skills-card-title">
                            <span className="skills-name">{skill.name}</span>
                            {installed_ && (
                              <span className="skills-chip">已安装</span>
                            )}
                          </div>
                          <button
                            type="button"
                            className={`skills-action-btn ${installed_ ? "" : "is-primary"}`}
                            onClick={() => handleInstallSkillHub(skill)}
                            disabled={installed_ || isInstalling || rankingLoading || skillhubSearching}
                          >
                            {isInstalling
                              ? installing?.progress
                              : installed_
                                ? "已安装"
                                : "安装"}
                          </button>
                        </div>
                        <div className="skills-card-desc">
                          <div className="skills-card-summary">{skill.summary}</div>
                          <div className="skills-card-meta">
                            {formatPublishDate(skill.created_at) && (
                              <span>{formatPublishDate(skill.created_at)} 上架</span>
                            )}
                            {skillAuthor(skill) && (
                              <span> · by {skillAuthor(skill)}</span>
                            )}
                            {skill.downloads != null && skill.downloads > 0 && (
                              <span> · {formatCount(skill.downloads)} 下载</span>
                            )}
                          </div>
                        </div>
                      </div>
                    );
                  })}
                </div>
          </div>
        )}
      </div>
    </div>
  );
}
