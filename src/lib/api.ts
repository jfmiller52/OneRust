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
  blueprintPath: string;
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
  blueprintPath: string;
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
