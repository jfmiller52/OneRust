import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Cpu,
  Download,
  FileCode2,
  FolderOpen,
  Play,
  Server,
  Shield,
} from "lucide-react";
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
  applyBlueprint,
  classifyBlueprint,
  downloadBundles,
  ensureOnecliReady,
  listModels,
  onDownloadProgress,
  onHostProgress,
  parseHosts,
  pickBlueprintFile,
  pickPackageDirectory,
  resolveMachineTypes,
  startUpdates,
  verifyBlueprint,
  type BlueprintInfo,
  type DownloadProgress,
  type HostProgress,
  type HostResult,
  type ModelInfo,
} from "@/lib/api";
import { cn } from "@/lib/utils";

type Mode = "firmware" | "blueprint";
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
    case "applied":
      return "text-ok";
    case "failed":
    case "error":
    case "mismatch":
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
  const [mode, setMode] = useState<Mode>("firmware");
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

  // Blueprint mode
  const [blueprintPath, setBlueprintPath] = useState("");
  const [blueprintInfo, setBlueprintInfo] = useState<BlueprintInfo | null>(null);
  const [packageDir, setPackageDir] = useState("firmware");
  const [blueprintRunning, setBlueprintRunning] = useState(false);
  const [blueprintResults, setBlueprintResults] = useState<HostResult[]>([]);
  const [blueprintProgress, setBlueprintProgress] = useState<
    Record<string, HostProgress>
  >({});

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
      if (mode === "blueprint") {
        setBlueprintProgress((prev) => ({ ...prev, [p.ip]: p }));
      } else {
        setHostProgress((prev) => ({ ...prev, [p.ip]: p }));
      }
    }).then((u) => unsubs.push(u));
    return () => {
      unsubs.forEach((u) => u());
    };
  }, [mode]);

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

  const chooseBlueprint = async () => {
    setError(null);
    try {
      const path = await pickBlueprintFile();
      if (!path) return;
      setBlueprintPath(path);
      const info = await classifyBlueprint(path);
      setBlueprintInfo(info);
    } catch (e) {
      setBlueprintInfo(null);
      setError(String(e));
    }
  };

  const choosePackageDir = async () => {
    try {
      const path = await pickPackageDirectory();
      if (path) setPackageDir(path);
    } catch (e) {
      setError(String(e));
    }
  };

  const runBlueprint = async () => {
    setError(null);
    if (!blueprintPath) {
      setError("Choose a blueprint file (.ini / .xml / .txt).");
      return;
    }
    const n = await validateHosts();
    if (!n) return;
    if (!password) {
      setError("XCC password is required.");
      return;
    }
    if (blueprintInfo?.needsPackageDir && !packageDir.trim()) {
      setError("Firmware XML blueprints need a package directory.");
      return;
    }

    setBlueprintRunning(true);
    setBlueprintResults([]);
    setBlueprintProgress({});
    try {
      await ensureOnecliReady();
      const res = await applyBlueprint({
        blueprintPath,
        hostsText,
        username,
        password,
        concurrency,
        logsDir,
        packageDir: blueprintInfo?.needsPackageDir ? packageDir : null,
        verifyBmcTls: verifyTls,
      });
      setBlueprintResults(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setBlueprintRunning(false);
    }
  };

  const runBlueprintVerify = async () => {
    setError(null);
    if (!blueprintPath) {
      setError("Choose a blueprint file (.ini / .xml / .txt).");
      return;
    }
    const n = await validateHosts();
    if (!n) return;
    if (!password) {
      setError("XCC password is required.");
      return;
    }
    if (blueprintInfo?.needsPackageDir && !packageDir.trim()) {
      setError("Firmware XML blueprints need a package directory to verify.");
      return;
    }

    setBlueprintRunning(true);
    setBlueprintResults([]);
    setBlueprintProgress({});
    try {
      await ensureOnecliReady();
      const res = await verifyBlueprint({
        blueprintPath,
        hostsText,
        username,
        password,
        concurrency,
        logsDir,
        packageDir: blueprintInfo?.needsPackageDir ? packageDir : null,
        verifyBmcTls: verifyTls,
      });
      setBlueprintResults(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setBlueprintRunning(false);
    }
  };

  const summary = useMemo(() => {
    const verified = results.filter((r) => r.outcome === "verified").length;
    const staged = results.filter((r) => r.outcome === "staged").length;
    const failed = results.filter((r) => r.outcome === "failed").length;
    const skipped = results.filter((r) => r.outcome === "skipped").length;
    return { verified, staged, failed, skipped };
  }, [results]);

  const blueprintSummary = useMemo(() => {
    const applied = blueprintResults.filter((r) => r.outcome === "applied").length;
    const verified = blueprintResults.filter((r) => r.outcome === "verified").length;
    const mismatch = blueprintResults.filter((r) => r.outcome === "mismatch").length;
    const failed = blueprintResults.filter((r) => r.outcome === "failed").length;
    return { applied, verified, mismatch, failed };
  }, [blueprintResults]);

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
            ThinkSystem V3/V4 · OneCLI · reboot + verify
          </span>
        </div>
        <div className="flex items-center gap-2 text-xs text-rack-400">
          <Shield className="size-3.5 text-signal" />
          {mode === "firmware"
            ? "Flash → reboot → OneCLI compare"
            : "Apply → restart hosts"}
        </div>
      </header>

      <div className="flex gap-1 border-b border-border px-6 py-2">
        <button
          type="button"
          onClick={() => {
            setMode("firmware");
            setError(null);
          }}
          className={cn(
            "rounded-md px-3 py-1.5 text-sm transition-colors",
            mode === "firmware"
              ? "bg-rack-800 text-rack-100"
              : "text-rack-400 hover:bg-rack-850 hover:text-rack-200"
          )}
        >
          Firmware update
        </button>
        <button
          type="button"
          onClick={() => {
            setMode("blueprint");
            setError(null);
          }}
          className={cn(
            "flex items-center gap-2 rounded-md px-3 py-1.5 text-sm transition-colors",
            mode === "blueprint"
              ? "bg-rack-800 text-rack-100"
              : "text-rack-400 hover:bg-rack-850 hover:text-rack-200"
          )}
        >
          <FileCode2 className="size-3.5" />
          Blueprint
        </button>
      </div>

      {mode === "firmware" && (
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
      )}

      <main className="flex-1 overflow-hidden px-6 py-5">
        {error && (
          <div className="mb-4 rounded-md border border-bad/40 bg-bad/10 px-3 py-2 text-sm text-bad">
            {error}
          </div>
        )}

        {mode === "blueprint" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div>
              <h2 className="text-lg font-medium">Apply blueprint</h2>
              <p className="mt-1 text-sm text-rack-400">
                Choose an{" "}
                <span className="font-mono text-rack-300">.ini</span> /{" "}
                <span className="font-mono text-rack-300">.txt</span> that can
                include BMC/UEFI settings and RAID policy in the{" "}
                <span className="font-medium text-rack-300">same file</span>{" "}
                (settings above a{" "}
                <span className="font-mono text-rack-300">#RAID</span> marker),
                a <span className="font-mono">config batch</span> of{" "}
                <span className="font-mono">set</span> commands, or a firmware
                compare <span className="font-mono text-rack-300">.xml</span>.
                OneRust runs settings → batch → RAID (body below{" "}
                <span className="font-mono">#RAID</span>), then force-restarts
                each host.
              </p>
            </div>

            <div className="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
              <div className="space-y-2">
                <Label>Blueprint file</Label>
                <Input
                  readOnly
                  value={blueprintPath}
                  placeholder="Select .ini / .xml / .txt…"
                  className="font-mono"
                />
              </div>
              <Button variant="outline" onClick={chooseBlueprint}>
                <FolderOpen className="size-4" />
                Browse
              </Button>
            </div>

            {blueprintInfo && (
              <div className="rounded-md border border-border bg-rack-850/60 px-3 py-2 text-sm">
                <span className="text-rack-100">{blueprintInfo.label}</span>
                <span className="mx-2 text-rack-600">·</span>
                <span className="font-mono text-xs text-rack-400">
                  {blueprintInfo.kind}
                </span>
                <span className="mx-2 text-rack-600">·</span>
                <span className="text-rack-400">{blueprintInfo.detail}</span>
              </div>
            )}

            {blueprintInfo?.needsPackageDir && (
              <div className="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
                <div className="space-y-2">
                  <Label htmlFor="pkgdir">Firmware package directory</Label>
                  <Input
                    id="pkgdir"
                    value={packageDir}
                    onChange={(e) => setPackageDir(e.target.value)}
                    className="font-mono"
                  />
                  <p className="text-xs text-rack-400">
                    Directory passed to OneCLI{" "}
                    <span className="font-mono">update flash --dir</span> with{" "}
                    <span className="font-mono">--comparexml</span>.
                  </p>
                </div>
                <Button variant="outline" onClick={choosePackageDir}>
                  <FolderOpen className="size-4" />
                  Browse
                </Button>
              </div>
            )}

            <div className="grid gap-3 sm:grid-cols-3">
              <div className="space-y-2">
                <Label htmlFor="bp-user">Username</Label>
                <Input
                  id="bp-user"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="bp-pass">Password</Label>
                <Input
                  id="bp-pass"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="bp-conc">Concurrency</Label>
                <Input
                  id="bp-conc"
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

            <div className="flex flex-wrap items-center gap-4">
              <div className="flex items-center gap-2">
                <Switch
                  id="bp-tls"
                  checked={verifyTls}
                  onCheckedChange={setVerifyTls}
                />
                <Label htmlFor="bp-tls">Verify BMC TLS certificates</Label>
              </div>
              <div className="space-y-0">
                <Label htmlFor="bp-logs" className="sr-only">
                  Logs directory
                </Label>
                <Input
                  id="bp-logs"
                  value={logsDir}
                  onChange={(e) => setLogsDir(e.target.value)}
                  className="h-8 w-40 font-mono text-xs"
                  title="Logs directory"
                />
              </div>
            </div>

            <div className="flex min-h-0 flex-1 flex-col gap-2">
              <Label htmlFor="bp-hosts">BMC IP addresses</Label>
              <Textarea
                id="bp-hosts"
                placeholder={"10.0.0.11\n10.0.0.12\nadmin:secret@10.0.0.13"}
                value={hostsText}
                onChange={(e) => setHostsText(e.target.value)}
                className="min-h-[120px] flex-1"
              />
              <p className="text-xs text-rack-400">
                {hostCount > 0
                  ? `${hostCount} host(s) parsed`
                  : "Paste IPs, one per line or comma-separated"}
              </p>
            </div>

            <div className="flex justify-end gap-2">
              <Button
                variant="outline"
                onClick={runBlueprintVerify}
                disabled={blueprintRunning}
              >
                {blueprintRunning ? "Working…" : "Verify against hosts"}
              </Button>
              <Button onClick={runBlueprint} disabled={blueprintRunning}>
                {blueprintRunning ? "Working…" : "Apply blueprint"}
              </Button>
            </div>

            <Separator />

            <ScrollArea className="min-h-0 flex-1 rounded-md border border-border bg-rack-850/60 p-3">
              <ul className="space-y-2 text-sm">
                {Object.values(blueprintProgress).length === 0 &&
                  blueprintResults.length === 0 && (
                    <li className="text-rack-400">
                      Per-host progress appears here after you apply or verify.
                    </li>
                  )}
                {(blueprintResults.length
                  ? blueprintResults.map((r) => ({
                      ip: r.ip,
                      serial: r.serial,
                      status: r.outcome,
                      detail: r.detail,
                    }))
                  : Object.values(blueprintProgress)
                ).map((row) => (
                  <li
                    key={row.ip}
                    className="grid grid-cols-[140px_100px_1fr] gap-2 border-b border-border/40 py-2 font-mono text-xs"
                  >
                    <span className="text-rack-100">{row.ip}</span>
                    <span className={statusColor(row.status)}>{row.status}</span>
                    <span className="truncate text-rack-400">
                      {("serial" in row && row.serial
                        ? `${row.serial} · `
                        : "") + row.detail}
                    </span>
                  </li>
                ))}
              </ul>
            </ScrollArea>

            {blueprintResults.length > 0 && (
              <div className="flex flex-wrap gap-6 font-mono text-sm">
                <span className="text-ok">applied {blueprintSummary.applied}</span>
                <span className="text-ok">verified {blueprintSummary.verified}</span>
                <span className="text-warn">mismatch {blueprintSummary.mismatch}</span>
                <span className="text-bad">failed {blueprintSummary.failed}</span>
              </div>
            )}
          </section>
        )}

        {mode === "firmware" && step === "models" && (
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

        {mode === "firmware" && step === "firmware" && (
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

        {mode === "firmware" && step === "targets" && (
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

        {mode === "firmware" && step === "run" && (
          <section className="mx-auto flex h-full max-w-4xl flex-col gap-4">
            <div className="flex flex-wrap items-end justify-between gap-4">
              <div>
                <h2 className="text-lg font-medium">Update, reboot &amp; verify</h2>
                <p className="mt-1 text-sm text-rack-400">
                  Stages the Update Bundle via OneCLI (`--bundle` / OnReset),
                  force-restarts each host, then runs OneCLI compare until no
                  further firmware updates are recommended. Logs:{" "}
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
