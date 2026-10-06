import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

export type ModelInfo = {
  index: number;
  name: string;
  machineTypes: string[];
  label: string;
};

export type DownloadProgress = {
  machineType: string;
  modelName: string;
  status: string;
  path?: string | null;
  error?: string | null;
  index: number;
  total: number;
};

export type HostProgress = {
  ip: string;
  serial?: string | null;
  status: string;
  detail: string;
};

export type HostConsoleLine = {
  ip: string;
  line: string;
};

export type HostResult = {
  ip: string;
  serial: string;
  outcome: string;
  detail: string;
};

export type BlueprintInfo = {
  path: string;
  kind: string;
  label: string;
  detail: string;
  needsPackageDir: boolean;
};

export async function listModels(): Promise<ModelInfo[]> {
  return invoke("list_models");
}

export async function resolveMachineTypes(
  selectedIndices: number[],
  extraMachineTypes: string[]
): Promise<string[]> {
  return invoke("resolve_machine_types", {
    selectedIndices,
    extraMachineTypes,
  });
}

export async function parseHosts(hostsText: string): Promise<string[]> {
  return invoke("parse_hosts", { hostsText });
}

export async function ensureOnecliReady(): Promise<string> {
  return invoke("ensure_onecli_ready");
}

export async function downloadBundles(args: {
  machineTypes: string[];
  firmwareDir: string;
  offline: boolean;
  forceReacquire?: boolean;
}): Promise<Record<string, string>> {
  return invoke("download_bundles", { request: args });
}

export async function startUpdates(args: {
  hostsText: string;
  username: string;
  password: string;
  concurrency: number;
  firmwareDir: string;
  logsDir: string;
  offline: boolean;
  verifyBmcTls: boolean;
  machineTypes: string[];
  bundleOverrides?: Record<string, string> | null;
  rebootAfterStage?: boolean;
  verifyWithCompare?: boolean;
  resetType?: string;
}): Promise<HostResult[]> {
  return invoke("start_updates", { request: args });
}

export async function classifyBlueprint(path: string): Promise<BlueprintInfo> {
  return invoke("classify_blueprint", { path });
}

export async function applyBlueprint(args: {
  blueprintPath?: string | null;
  raidPath?: string | null;
  hostsText: string;
  username: string;
  password: string;
  concurrency: number;
  logsDir: string;
  packageDir?: string | null;
  verifyBmcTls: boolean;
  kindOverride?: string | null;
  applytime?: string;
  rebootAfterApply?: boolean;
  resetType?: string;
  rebootTimeoutSecs?: number;
}): Promise<HostResult[]> {
  return invoke("apply_blueprint", { request: args });
}

export async function verifyBlueprint(args: {
  blueprintPath?: string | null;
  raidPath?: string | null;
  hostsText: string;
  username: string;
  password: string;
  concurrency: number;
  logsDir: string;
  packageDir?: string | null;
  verifyBmcTls: boolean;
  kindOverride?: string | null;
  applytime?: string;
}): Promise<HostResult[]> {
  return invoke("verify_blueprint", { request: args });
}

export async function pickBlueprintFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    filters: [
      {
        name: "Blueprints",
        extensions: ["ini", "xml", "txt"],
      },
    ],
  });
  if (typeof selected === "string") return selected;
  return null;
}

export async function pickRaidFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    filters: [
      {
        name: "RAID settings",
        extensions: ["ini"],
      },
    ],
  });
  if (typeof selected === "string") return selected;
  return null;
}

export async function pickPackageDirectory(): Promise<string | null> {
  const selected = await open({
    directory: true,
    multiple: false,
  });
  if (typeof selected === "string") return selected;
  return null;
}

export async function cancelJobs(): Promise<void> {
  return invoke("cancel_jobs");
}

export async function loadHostsFile(path: string): Promise<string> {
  return invoke("load_hosts_file", { path });
}

export async function openPath(path: string): Promise<void> {
  return invoke("open_path", { path });
}

export async function pickHostsFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    filters: [{ name: "Host lists", extensions: ["txt", "csv", "list"] }],
  });
  if (typeof selected === "string") return selected;
  return null;
}

export function onDownloadProgress(
  handler: (p: DownloadProgress) => void
): Promise<UnlistenFn> {
  return listen<DownloadProgress>("download-progress", (e) => handler(e.payload));
}

export function onHostProgress(
  handler: (p: HostProgress) => void
): Promise<UnlistenFn> {
  return listen<HostProgress>("host-progress", (e) => handler(e.payload));
}

export function onHostConsole(
  handler: (p: HostConsoleLine) => void
): Promise<UnlistenFn> {
  return listen<HostConsoleLine>("host-console", (e) => handler(e.payload));
}

export type AppUpdateInfo = {
  version: string;
  currentVersion: string;
  notes: string | null;
  date: string | null;
};

/** Check GitHub Releases for a newer OneRust build (via signed latest.json). */
export async function checkForAppUpdate(): Promise<AppUpdateInfo | null> {
  const { check } = await import("@tauri-apps/plugin-updater");
  const { getVersion } = await import("@tauri-apps/api/app");
  const currentVersion = await getVersion();
  const update = await check();
  if (!update) return null;
  return {
    version: update.version,
    currentVersion,
    notes: update.body ?? null,
    date: update.date ?? null,
  };
}

/**
 * Download and install the latest update, then relaunch.
 * On Windows the installer runs in passive mode and the app exits.
 */
export async function downloadAndInstallAppUpdate(
  onProgress?: (downloaded: number, total: number | undefined) => void
): Promise<void> {
  const { check } = await import("@tauri-apps/plugin-updater");
  const { relaunch } = await import("@tauri-apps/plugin-process");
  const update = await check();
  if (!update) {
    throw new Error("No update available");
  }
  let downloaded = 0;
  let contentLength: number | undefined;
  await update.downloadAndInstall((event) => {
    switch (event.event) {
      case "Started":
        contentLength = event.data.contentLength;
        onProgress?.(0, contentLength);
        break;
      case "Progress":
        downloaded += event.data.chunkLength;
        onProgress?.(downloaded, contentLength);
        break;
      case "Finished":
        onProgress?.(downloaded, contentLength);
        break;
    }
  });
  await relaunch();
}
