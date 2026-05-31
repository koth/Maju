import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";

export interface AppUpdateInfo {
  currentVersion: string;
  version: string;
  date: string | null;
  body: string | null;
}

export interface AppUpdateProgress {
  phase: "started" | "progress" | "finished";
  downloadedBytes: number;
  contentLength: number | null;
}

type PendingUpdate = Awaited<ReturnType<typeof check>>;

let pendingUpdate: PendingUpdate = null;

export async function getCurrentAppVersion(): Promise<string> {
  return getVersion();
}

export async function checkForAppUpdate(): Promise<AppUpdateInfo | null> {
  const [currentVersion, update] = await Promise.all([getVersion(), check()]);
  pendingUpdate = update;

  if (!update) {
    return null;
  }

  return {
    currentVersion,
    version: update.version,
    date: update.date ?? null,
    body: update.body ?? null,
  };
}

export async function installPendingAppUpdate(
  onProgress?: (progress: AppUpdateProgress) => void,
): Promise<void> {
  if (!pendingUpdate) {
    throw new Error("没有可安装的更新");
  }

  let downloadedBytes = 0;
  let contentLength: number | null = null;

  await pendingUpdate.downloadAndInstall((event) => {
    switch (event.event) {
      case "Started":
        downloadedBytes = 0;
        contentLength = event.data.contentLength ?? null;
        onProgress?.({ phase: "started", downloadedBytes, contentLength });
        break;
      case "Progress":
        downloadedBytes += event.data.chunkLength;
        onProgress?.({ phase: "progress", downloadedBytes, contentLength });
        break;
      case "Finished":
        onProgress?.({ phase: "finished", downloadedBytes, contentLength });
        break;
    }
  });

  pendingUpdate = null;
  await relaunch();
}
