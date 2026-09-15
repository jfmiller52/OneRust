import { useCallback, useEffect, useMemo, useState } from "react";
import { Cpu, Download, Play, Server, Shield } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  downloadBundles,
  ensureOnecliReady,
  listModels,
  onDownloadProgress,
  onHostProgress,
  parseHosts,
  resolveMachineTypes,
  startUpdates,
  type DownloadProgress,
  type HostProgress,
  type HostResult,
  type ModelInfo,
} from "@/lib/api";
import { cn } from "@/lib/utils";

type Step = "models" | "firmware" | "targets" | "run";

const STEPS: { id: Step; label: string; icon: typeof Cpu }[] = [
  { id: "models", label: "Models", icon: Cpu },
  { id: "firmware", label: "Firmware", icon: Download },
  { id: "targets", label: "Targets", icon: Server },
  { id: "run", label: "Update", icon: Play },
];

function statusColor(status: string) {
  switch (status) {
    case "ready":
    case "staged":
    case "verified":
      return "text-ok";
    case "failed":
    case "error":
      return "text-bad";
    case "skipped":
    case "queued":
      return "text-warn";
    case "running":
    case "staging":
    case "rebooting":
    case "applying":
    case "verifying":
    case "downloading":
    case "checking":
      return "text-signal";
    default:
      return "text-rack-400";
  }
}

export default function App() {
  const [step, setStep] = useState<Step>("models");
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [extraMts, setExtraMts] = useState("");
  const [machineTypes, setMachineTypes] = useState<string[]>([]);
  const [bundles, setBundles] = useState<Record<string, string>>({});
  const [downloadLog, setDownloadLog] = useState<DownloadProgress[]>([]);
  const [downloading, setDownloading] = useState(false);
  const [offline, setOffline] = useState(false);
  const [firmwareDir, setFirmwareDir] = useState("firmware");
  const [logsDir, setLogsDir] = useState("logs");
  const [username, setUsername] = useState("USERID");
  const [password, setPassword] = useState("");
  const [concurrency, setConcurrency] = useState(4);
  const [verifyTls, setVerifyTls] = useState(false);
  const [hostsText, setHostsText] = useState("");
  const [hostCount, setHostCount] = useState(0);
  const [running, setRunning] = useState(false);
  const [hostProgress, setHostProgress] = useState<Record<string, HostProgress>>({});
  const [results, setResults] = useState<HostResult[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [onecliStatus, setOnecliStatus] = useState<string>("checking");

  useEffect(() => {
    listModels()
      .then(setModels)
      .catch((e) => setError(String(e)));
  }, []);

  // First run: ensure OneCLI exists beside the executable (download if needed).
  useEffect(() => {
    let cancelled = false;
    setOnecliStatus("checking");
    ensureOnecliReady()
      .then((path) => {
        if (!cancelled) setOnecliStatus(`ready: ${path}`);
      })
      .catch((e) => {
        if (!cancelled) {
          setOnecliStatus("error");
          setError(`OneCLI setup failed: ${String(e)}`);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let unsubs: (() => void)[] = [];
    onDownloadProgress((p) => {
      setDownloadLog((prev) => {
        const next = [...prev.filter((x) => x.machineType !== p.machineType), p];
        return next.sort((a, b) => a.index - b.index);
      });
      if (p.machineType === "ONECLI") {
        setOnecliStatus(p.status);
      }
    }).then((u) => unsubs.push(u));
    onHostProgress((p) => {
      setHostProgress((prev) => ({ ...prev, [p.ip]: p }));
    }).then((u) => unsubs.push(u));
    return () => {
      unsubs.forEach((u) => u());
    };
  }, []);

  const toggleModel = (index: number) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  };

  const canAdvanceFromModels = selected.size > 0 || extraMts.trim().length > 0;

  const goFirmware = async () => {
    setError(null);
    try {
      const extras = extraMts
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const mts = await resolveMachineTypes([...selected], extras);
      if (mts.length === 0) {
        setError("Select at least one model or machine type.");
        return;
      }
      setMachineTypes(mts);
      setStep("firmware");
    } catch (e) {
      setError(String(e));
    }
  };

  const runDownload = async () => {
    setError(null);
    setDownloading(true);
    setDownloadLog([]);
    setBundles({});
    try {
      if (!offline) {
        await ensureOnecliReady();
      }
      const map = await downloadBundles({
        machineTypes,
        firmwareDir,
        offline,
      });
      setBundles(map);
    } catch (e) {
      setError(String(e));
    } finally {
      setDownloading(false);
    }
  };

  const goTargets = () => {
    if (Object.keys(bundles).length === 0) {
      setError("Download firmware bundles before continuing.");
      return;
    }
    setError(null);
    setStep("targets");
  };

  const validateHosts = useCallback(async () => {
    try {
      const ips = await parseHosts(hostsText);
      setHostCount(ips.length);
      setError(null);
      return ips.length;
    } catch (e) {
      setHostCount(0);
      setError(String(e));
      return 0;
    }
  }, [hostsText]);

  const goRun = async () => {
    const n = await validateHosts();
    if (!n) return;
    if (!password) {
      setError("XCC password is required.");
      return;
    }
    setError(null);
    setStep("run");
  };

  const runUpdates = async () => {
    setError(null);
    setRunning(true);
    setResults([]);
    setHostProgress({});
    try {
      const res = await startUpdates({
        hostsText,
        username,
        password,
        concurrency,
        firmwareDir,
        logsDir,
        offline,
        verifyBmcTls: verifyTls,
        machineTypes,
        bundleOverrides: bundles,
      });
      setResults(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  const summary = useMemo(() => {
    const verified = results.filter((r) => r.outcome === "verified").length;
    const staged = results.filter((r) => r.outcome === "staged").length;
    const failed = results.filter((r) => r.outcome === "failed").length;
    const skipped = results.filter((r) => r.outcome === "skipped").length;
    return { verified, staged, failed, skipped };
  }, [results]);

  const downloadPct =
    machineTypes.length === 0
      ? 0
      : Math.round(
          (downloadLog.filter((d) => d.status === "ready").length /
            machineTypes.length) *
            100
        );

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-border px-6 py-4">
        <div className="flex items-baseline gap-3">
          <h1 className="text-2xl font-semibold tracking-tight text-rack-100">
            OneRust
          </h1>
          <span className="font-mono text-xs text-rack-400">
            ThinkSystem V3/V4 · Redfish · reboot + verify
          </span>
        </div>
        <div className="flex items-center gap-2 text-xs text-rack-400">
          <Shield className="size-3.5 text-signal" />
          Stage → reboot → OneCLI compare
        </div>
      </header>

      <nav className="flex gap-1 border-b border-border px-6 py-3">
        {STEPS.map((s, i) => {
          const Icon = s.icon;
          const active = step === s.id;
          const idx = STEPS.findIndex((x) => x.id === step);
          const done = i < idx;
          return (
            <button
              key={s.id}
              type="button"
              onClick={() => {
                if (done || active) setStep(s.id);
              }}
              className={cn(
                "flex items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors",
                active && "bg-rack-800 text-rack-100",
                !active && done && "text-rack-300 hover:bg-rack-850",
                !active && !done && "text-rack-600 cursor-default"
              )}
            >
              <Icon className="size-4" />
              <span className="font-mono text-[10px] text-rack-500">
                {String(i + 1).padStart(2, "0")}
              </span>
              {s.label}
            </button>
          );
        })}
      </nav>

      <main className="flex-1 overflow-hidden px-6 py-5">
        {error && (
          <div className="mb-4 rounded-md border border-bad/40 bg-bad/10 px-3 py-2 text-sm text-bad">
            {error}
          </div>
        )}

        {step === "models" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div>
              <h2 className="text-lg font-medium">Select models to update</h2>
              <p className="mt-1 text-sm text-rack-400">
                Choose ThinkSystem V3/V4 families. Machine types are resolved
                automatically for firmware download.
              </p>
            </div>
            <ScrollArea className="min-h-0 flex-1 rounded-md border border-border bg-rack-850/60 p-3">
              <div className="grid gap-2 sm:grid-cols-2">
                {models.map((m) => {
                  const checked = selected.has(m.index);
                  return (
                    <label
                      key={m.index}
                      className={cn(
                        "flex cursor-pointer items-start gap-3 rounded-md border px-3 py-3 transition-colors",
                        checked
                          ? "border-signal/50 bg-signal/5"
                          : "border-transparent hover:bg-rack-800/80"
                      )}
                    >
                      <Checkbox
                        checked={checked}
                        onCheckedChange={() => toggleModel(m.index)}
                        className="mt-0.5"
                      />
                      <span>
                        <span className="block text-sm font-medium">{m.name}</span>
                        <span className="mt-0.5 block font-mono text-xs text-rack-400">
                          {m.machineTypes.join(" · ")}
                        </span>
                      </span>
                    </label>
                  );
                })}
              </div>
            </ScrollArea>
            <div className="space-y-2">
              <Label htmlFor="extra">Extra machine types</Label>
              <Input
                id="extra"
                placeholder="7D76, 7DG8"
                value={extraMts}
                onChange={(e) => setExtraMts(e.target.value)}
                className="font-mono"
              />
            </div>
            <div className="flex justify-end">
              <Button disabled={!canAdvanceFromModels} onClick={goFirmware}>
                Continue to firmware
              </Button>
            </div>
          </section>
        )}

        {step === "firmware" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div className="flex flex-wrap items-end justify-between gap-4">
              <div>
                <h2 className="text-lg font-medium">Download Update Bundles</h2>
                <p className="mt-1 text-sm text-rack-400">
                  Uses Lenovo OneCLI from{" "}
                  <span className="font-mono text-rack-300">OneCLI/</span> beside
                  the app (
                  <span className="font-mono text-rack-300">update acquire --zip</span>
                  ) for{" "}
                  <span className="font-mono text-rack-300">
                    {machineTypes.join(", ")}
                  </span>
                  . On first run, OneCLI is downloaded automatically if missing
                  {onecliStatus.startsWith("ready")
                    ? " (ready)."
                    : onecliStatus === "downloading" || onecliStatus === "checking"
                      ? ` (${onecliStatus}…).`
                      : "."}
                </p>
              </div>
              <div className="flex items-center gap-3">
                <div className="flex items-center gap-2">
                  <Switch
                    id="offline"
                    checked={offline}
                    onCheckedChange={setOffline}
                  />
                  <Label htmlFor="offline">Offline (local ZIPs only)</Label>
                </div>
                <Button onClick={runDownload} disabled={downloading}>
                  {downloading ? "Acquiring…" : "Acquire with OneCLI"}
                </Button>
              </div>
            </div>

            <div className="grid gap-3 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="fwdir">Firmware directory</Label>
                <Input
                  id="fwdir"
                  value={firmwareDir}
                  onChange={(e) => setFirmwareDir(e.target.value)}
                  className="font-mono"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="logdir">Logs directory</Label>
                <Input
                  id="logdir"
                  value={logsDir}
                  onChange={(e) => setLogsDir(e.target.value)}
                  className="font-mono"
                />
              </div>
            </div>

            <Progress value={downloadPct} />

            <ScrollArea className="min-h-0 flex-1 rounded-md border border-border bg-rack-850/60 p-3">
              <ul className="space-y-2 font-mono text-xs">
                {machineTypes.map((mt) => {
                  const row = downloadLog.find((d) => d.machineType === mt);
                  const path = bundles[mt] ?? row?.path;
                  return (
                    <li
                      key={mt}
                      className="flex flex-wrap items-baseline justify-between gap-2 border-b border-border/50 pb-2"
                    >
                      <span>
                        <span className="text-rack-100">{mt}</span>{" "}
                        <span className={statusColor(row?.status ?? "idle")}>
                          {row?.status ?? "waiting"}
                        </span>
                      </span>
                      <span className="truncate text-rack-400">
                        {row?.error ?? path ?? "—"}
                      </span>
                    </li>
                  );
                })}
              </ul>
            </ScrollArea>

            <div className="flex justify-between">
              <Button variant="outline" onClick={() => setStep("models")}>
                Back
              </Button>
              <Button
                disabled={Object.keys(bundles).length === 0 || downloading}
                onClick={goTargets}
              >
                Continue to targets
              </Button>
            </div>
          </section>
        )}

        {step === "targets" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div>
              <h2 className="text-lg font-medium">BMC credentials & hosts</h2>
              <p className="mt-1 text-sm text-rack-400">
                Shared XCC login for all targets. Per-host override:{" "}
                <span className="font-mono">user:pass@ip</span>
              </p>
            </div>

            <div className="grid gap-3 sm:grid-cols-3">
              <div className="space-y-2">
                <Label htmlFor="user">Username</Label>
                <Input
                  id="user"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="pass">Password</Label>
                <Input
                  id="pass"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="conc">Concurrency</Label>
                <Input
                  id="conc"
                  type="number"
                  min={1}
                  max={50}
                  value={concurrency}
                  onChange={(e) =>
                    setConcurrency(Math.max(1, Number(e.target.value) || 1))
                  }
                />
              </div>
            </div>

            <div className="flex items-center gap-2">
              <Switch
                id="tls"
                checked={verifyTls}
                onCheckedChange={setVerifyTls}
              />
              <Label htmlFor="tls">Verify BMC TLS certificates</Label>
            </div>

            <div className="flex min-h-0 flex-1 flex-col gap-2">
              <Label htmlFor="hosts">BMC IP addresses</Label>
              <Textarea
                id="hosts"
                placeholder={"10.0.0.11\n10.0.0.12\nadmin:secret@10.0.0.13"}
                value={hostsText}
                onChange={(e) => setHostsText(e.target.value)}
                className="min-h-[180px] flex-1"
              />
              <p className="text-xs text-rack-400">
                {hostCount > 0
                  ? `${hostCount} host(s) parsed`
                  : "Paste IPs, one per line or comma-separated"}
              </p>
            </div>

            <div className="flex justify-between">
              <Button variant="outline" onClick={() => setStep("firmware")}>
                Back
              </Button>
              <Button onClick={goRun}>Continue to update</Button>
            </div>
          </section>
        )}

        {step === "run" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div className="flex flex-wrap items-end justify-between gap-4">
              <div>
                <h2 className="text-lg font-medium">Update, reboot &amp; verify</h2>
                <p className="mt-1 text-sm text-rack-400">
                  Stages the Update Bundle (OnReset), force-restarts each host,
                  then runs OneCLI compare until no further firmware updates are
                  recommended. Logs:{" "}
                  <span className="font-mono text-rack-300">{logsDir}/</span>
                </p>
              </div>
              <Button onClick={runUpdates} disabled={running}>
                {running ? "Updating…" : "Start update"}
              </Button>
            </div>

            <Separator />

            <ScrollArea className="min-h-0 flex-1 rounded-md border border-border bg-rack-850/60 p-3">
              <ul className="space-y-2 text-sm">
                {Object.values(hostProgress).length === 0 &&
                  results.length === 0 && (
                    <li className="text-rack-400">
                      Host progress will appear here when you start.
                    </li>
                  )}
                {(results.length
                  ? results.map((r) => ({
                      ip: r.ip,
                      serial: r.serial,
                      status: r.outcome,
                      detail: r.detail,
                    }))
                  : Object.values(hostProgress)
                ).map((row) => (
                  <li
                    key={row.ip}
                    className="grid grid-cols-[140px_120px_1fr] gap-2 border-b border-border/40 py-2 font-mono text-xs"
                  >
                    <span className="text-rack-100">{row.ip}</span>
                    <span className={statusColor(row.status)}>{row.status}</span>
                    <span className="truncate text-rack-400">
                      {(row.serial ? `${row.serial} · ` : "") + row.detail}
                    </span>
                  </li>
                ))}
              </ul>
            </ScrollArea>

            {results.length > 0 && (
              <div className="flex flex-wrap gap-6 font-mono text-sm">
                <span className="text-ok">verified {summary.verified}</span>
                <span className="text-signal">staged {summary.staged}</span>
                <span className="text-bad">failed {summary.failed}</span>
                <span className="text-warn">skipped {summary.skipped}</span>
              </div>
            )}

            <div className="flex justify-between">
              <Button
                variant="outline"
                onClick={() => setStep("targets")}
                disabled={running}
              >
                Back
              </Button>
            </div>
          </section>
        )}
      </main>
    </div>
  );
}
