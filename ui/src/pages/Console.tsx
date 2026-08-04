import { useEffect, useRef } from "react";
import { useNavigate, useParams } from "react-router-dom";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { useVm } from "../api/hooks";
import { authToken } from "../api/client";

// Interactive serial console (design section 25): browser <-WS-> control
// <-gRPC-> agent <-> VM serial.
export function Console() {
  const { id } = useParams();
  const navigate = useNavigate();
  const vm = useVm(id);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!containerRef.current || !id) return;

    const term = new Terminal({
      convertEol: true,
      fontFamily: "ui-monospace, Menlo, Consolas, monospace",
      fontSize: 13,
      theme: { background: "#0b0e14" },
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
  }, [id]);

  return (
    <Stack spacing={2} sx={{ height: "calc(100vh - 120px)" }}>
      <Stack direction="row" alignItems="center" spacing={2}>
        <Button startIcon={<ArrowBackIcon />} onClick={() => navigate(`/vms/${id}`)}>
          Back
        </Button>
        <Typography variant="h5">Console — {vm.data?.name ?? id}</Typography>
      </Stack>
      <Box
        ref={containerRef}
        sx={{ flexGrow: 1, bgcolor: "#0b0e14", p: 1, borderRadius: 1, overflow: "hidden" }}
      />
    </Stack>
  );
}
