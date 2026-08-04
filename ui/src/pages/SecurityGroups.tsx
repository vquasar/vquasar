import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import IconButton from "@mui/material/IconButton";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Table from "@mui/material/Table";
import TableBody from "@mui/material/TableBody";
import TableCell from "@mui/material/TableCell";
import TableHead from "@mui/material/TableHead";
import TableRow from "@mui/material/TableRow";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import Accordion from "@mui/material/Accordion";
import AccordionSummary from "@mui/material/AccordionSummary";
import AccordionDetails from "@mui/material/AccordionDetails";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ShieldIcon from "@mui/icons-material/Shield";
import {
  useAddSgRule,
  useCreateSecurityGroup,
  useDeleteSecurityGroup,
  useDeleteSgRule,
  useSecurityGroups,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import type { CreateRuleRequest, SecurityGroup, SecurityGroupRule } from "../api/types";

function ruleLabel(r: SecurityGroupRule): string {
  const proto = r.protocol === "any" ? "all" : r.protocol;
  const ports =
    r.protocol === "tcp" || r.protocol === "udp"
      ? r.port_min != null || r.port_max != null
        ? ` :${r.port_min ?? 0}${r.port_max && r.port_max !== r.port_min ? `-${r.port_max}` : ""}`
        : " :all"
      : "";
  const from = r.remote_cidr ? ` from ${r.remote_cidr}` : " from any";
  return `${proto}${ports}${from}`;
}

function AddRuleForm({ groupId }: { groupId: string }) {
  const add = useAddSgRule();
  const [protocol, setProtocol] = useState("tcp");
  const [portMin, setPortMin] = useState("");
  const [portMax, setPortMax] = useState("");
  const [ethertype, setEthertype] = useState("IPv4");
  const [cidr, setCidr] = useState("");

  const submit = () => {
    const body: CreateRuleRequest = {
      direction: "ingress",
      ethertype,
      protocol,
      port_min: portMin ? Number(portMin) : null,
      port_max: portMax ? Number(portMax) : portMin ? Number(portMin) : null,
      remote_cidr: cidr.trim() || null,
    };
    add.mutate({ id: groupId, body }, { onSuccess: () => { setPortMin(""); setPortMax(""); setCidr(""); } });
  };
  const hasPorts = protocol === "tcp" || protocol === "udp";

  return (
    <Stack direction="row" spacing={1} alignItems="center" sx={{ flexWrap: "wrap", gap: 1 }}>
      <TextField select size="small" label="Protocol" value={protocol} onChange={(e) => setProtocol(e.target.value)} sx={{ minWidth: 100 }}>
        {["tcp", "udp", "icmp", "any"].map((p) => <MenuItem key={p} value={p}>{p}</MenuItem>)}
      </TextField>
      <TextField select size="small" label="Family" value={ethertype} onChange={(e) => setEthertype(e.target.value)} sx={{ minWidth: 90 }}>
        {["IPv4", "IPv6"].map((p) => <MenuItem key={p} value={p}>{p}</MenuItem>)}
      </TextField>
      <TextField size="small" label="Port (min)" value={portMin} disabled={!hasPorts} onChange={(e) => setPortMin(e.target.value)} sx={{ width: 100 }} />
      <TextField size="small" label="Port (max)" value={portMax} disabled={!hasPorts} onChange={(e) => setPortMax(e.target.value)} sx={{ width: 100 }} />
      <TextField size="small" label="From CIDR (optional)" value={cidr} onChange={(e) => setCidr(e.target.value)} placeholder="0.0.0.0/0" sx={{ minWidth: 160 }} />
      <Button startIcon={<AddIcon />} variant="outlined" size="small" disabled={add.isPending} onClick={submit}>
        Add rule
      </Button>
      {add.error && <Alert severity="error" sx={{ width: "100%" }}>{(add.error as Error).message}</Alert>}
    </Stack>
  );
}

function GroupCard({ group, canManage }: { group: SecurityGroup; canManage: boolean }) {
  const delRule = useDeleteSgRule();
  const ingress = group.rules.filter((r) => r.direction === "ingress");
  return (
    <Accordion>
      <AccordionSummary expandIcon={<ExpandMoreIcon />}>
        <Stack direction="row" spacing={1} alignItems="center" sx={{ width: "100%" }}>
          <ShieldIcon fontSize="small" color="primary" />
          <Typography sx={{ fontWeight: 600 }}>{group.name}</Typography>
          {group.description && (
            <Typography variant="body2" color="text.secondary">
              {group.description}
            </Typography>
          )}
          <Box sx={{ flexGrow: 1 }} />
          <Chip size="small" label={`${ingress.length} ingress rule${ingress.length === 1 ? "" : "s"}`} />
        </Stack>
      </AccordionSummary>
      <AccordionDetails>
        <Typography variant="caption" color="text.secondary">
          Default-deny ingress, allow egress, stateful. Rules below open inbound traffic.
        </Typography>
        <Table size="small" sx={{ mt: 1 }}>
          <TableHead>
            <TableRow>
              <TableCell>Allow</TableCell>
              <TableCell>Family</TableCell>
              <TableCell align="right"></TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {ingress.map((r) => (
              <TableRow key={r.id}>
                <TableCell>{ruleLabel(r)}</TableCell>
                <TableCell>{r.ethertype}</TableCell>
                <TableCell align="right">
                  {canManage && (
                    <IconButton size="small" onClick={() => delRule.mutate({ id: group.id, ruleId: r.id })}>
                      <DeleteIcon fontSize="small" />
                    </IconButton>
                  )}
                </TableCell>
              </TableRow>
            ))}
            {ingress.length === 0 && (
              <TableRow>
                <TableCell colSpan={3}>
                  <Typography variant="body2" color="text.secondary">
                    No ingress rules — all inbound denied.
                  </Typography>
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
        {canManage && (
          <Box sx={{ mt: 2 }}>
            <AddRuleForm groupId={group.id} />
          </Box>
        )}
      </AccordionDetails>
    </Accordion>
  );
}

function CreateDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateSecurityGroup();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>New security group</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField label="Description" value={description} onChange={(e) => setDescription(e.target.value)} />
          {create.error && <Alert severity="error">{(create.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!name || create.isPending}
          onClick={() => create.mutate({ name, description: description || null }, { onSuccess: onClose })}
        >
          Create
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function SecurityGroups() {
  const groups = useSecurityGroups();
  const del = useDeleteSecurityGroup();
  const { can } = usePermissions();
  const canManage = can("network:update");
  const [creating, setCreating] = useState(false);

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Security groups</Typography>
        {can("network:create") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setCreating(true)}>
            New group
          </Button>
        )}
      </Stack>
      <Typography variant="body2" color="text.secondary">
        Attach a group to a VM NIC to apply stateful filtering. A NIC with no group is unfiltered.
      </Typography>
      {(groups.error || del.error) && (
        <Alert severity="error">{((groups.error || del.error) as Error).message}</Alert>
      )}
      <Box>
        {(groups.data ?? []).map((g) => (
          <Stack key={g.id} direction="row" alignItems="flex-start" spacing={1}>
            <Box sx={{ flexGrow: 1 }}>
              <GroupCard group={g} canManage={canManage} />
            </Box>
            {can("network:delete") && (
              <IconButton sx={{ mt: 1 }} onClick={() => del.mutate(g.id)} title="Delete group">
                <DeleteIcon />
              </IconButton>
            )}
          </Stack>
        ))}
        {groups.data?.length === 0 && (
          <Typography color="text.secondary">No security groups yet.</Typography>
        )}
      </Box>
      {creating && <CreateDialog onClose={() => setCreating(false)} />}
    </Stack>
  );
}
