import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
}): Promise<HostResult[]> {
  return invoke("start_updates", { request: args });
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
