// Interactive serial console (design section 25): browser <-WS-> control
// <-gRPC-> agent <-> VM serial. Not redesigned in the handoff — kept functional
// and restyled from the token set.

import { useEffect, useRef } from "react";
import { Link, useParams } from "react-router-dom";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { useVm } from "../api/hooks";
import { authToken } from "../api/client";
import { PageHeader } from "../ui/kit";
import { useThemeMode } from "../theme/ThemeMode";
import { useCrumb } from "../components/Breadcrumb";

export function Console() {
  const { id } = useParams();
  const vm = useVm(id);
  const { mode } = useThemeMode();
  useCrumb(vm.data ? `${vm.data.name} · console` : "console");
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!containerRef.current || !id) return;

    const term = new Terminal({
      convertEol: true,
      fontFamily: '"JetBrains Mono", ui-monospace, Menlo, Consolas, monospace',
      fontSize: 12.5,
      theme:
        mode === "light"
          ? { background: "#F4F7FB", foreground: "#0B0F16", cursor: "#1E6FD9" }
          : { background: "#0D1219", foreground: "#F4F7FB", cursor: "#4F9CF9" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerRef.current);
    fit.fit();

    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    // Browsers can't set headers on a WebSocket handshake, so the bearer token
    // rides as a query param; the control plane authorizes it (vm:console).
    const token = authToken();
    const qs = token ? `?access_token=${encodeURIComponent(token)}` : "";
    const ws = new WebSocket(`${proto}//${location.host}/api/v1/vms/${id}/console${qs}`);
    ws.binaryType = "arraybuffer";

    const encoder = new TextEncoder();
    ws.onopen = () => term.writeln("\x1b[2m[connected — press Enter]\x1b[0m");
    ws.onclose = () => term.writeln("\r\n\x1b[2m[console closed]\x1b[0m");
    ws.onmessage = (ev) => {
      if (ev.data instanceof ArrayBuffer) term.write(new Uint8Array(ev.data));
      else term.write(ev.data as string);
    };
    const onData = term.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(encoder.encode(data));
    });

    const onResize = () => fit.fit();
    window.addEventListener("resize", onResize);

    return () => {
      window.removeEventListener("resize", onResize);
      onData.dispose();
      ws.close();
      term.dispose();
    };
  }, [id, mode]);

  return (
    <>
      <PageHeader
        back={
          <Link to={`/vms/${id}`} className="vq-backlink">
            ← {vm.data?.name ?? "Virtual machine"}
          </Link>
        }
        title="Serial console"
        subline={vm.data ? `${vm.data.id} · ${vm.data.phase}` : undefined}
      />
      <div
        ref={containerRef}
        style={{
          flex: 1,
          minHeight: 460,
          background: "var(--vq-inset)",
          border: "1px solid var(--vq-line)",
          borderRadius: "var(--vq-radius-card)",
          padding: 12,
          overflow: "hidden",
        }}
      />
    </>
  );
}
