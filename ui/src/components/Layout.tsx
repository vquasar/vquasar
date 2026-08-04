import { ReactNode, useState } from "react";
import { Link as RouterLink, useLocation } from "react-router-dom";
import AppBar from "@mui/material/AppBar";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Drawer from "@mui/material/Drawer";
import List from "@mui/material/List";
import ListItemButton from "@mui/material/ListItemButton";
import ListItemIcon from "@mui/material/ListItemIcon";
import ListItemText from "@mui/material/ListItemText";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import Toolbar from "@mui/material/Toolbar";
import Typography from "@mui/material/Typography";
import DashboardIcon from "@mui/icons-material/Dashboard";
import DnsIcon from "@mui/icons-material/Dns";
import ComputerIcon from "@mui/icons-material/Computer";
import LanIcon from "@mui/icons-material/Lan";
import ShieldIcon from "@mui/icons-material/Shield";
import TaskAltIcon from "@mui/icons-material/TaskAlt";
import NotificationsIcon from "@mui/icons-material/Notifications";
import AlbumIcon from "@mui/icons-material/Album";
import ViewQuiltIcon from "@mui/icons-material/ViewQuilt";
import SecurityIcon from "@mui/icons-material/Security";
import AccountCircleIcon from "@mui/icons-material/AccountCircle";
import { useAuth } from "../auth/AuthProvider";
import { usePermissions } from "../auth/permissions";

const DRAWER_WIDTH = 220;

const NAV = [
  { to: "/", label: "Dashboard", icon: <DashboardIcon /> },
  { to: "/hosts", label: "Hosts", icon: <DnsIcon /> },
  { to: "/vms", label: "Virtual Machines", icon: <ComputerIcon /> },
  { to: "/images", label: "Images", icon: <AlbumIcon /> },
  { to: "/templates", label: "Templates", icon: <ViewQuiltIcon /> },
  { to: "/networks", label: "Networks", icon: <LanIcon /> },
  { to: "/security-groups", label: "Security Groups", icon: <ShieldIcon /> },
  { to: "/tasks", label: "Tasks", icon: <TaskAltIcon /> },
  { to: "/events", label: "Events", icon: <NotificationsIcon /> },
];

function UserMenu() {
  const { enabled, profile, logout } = useAuth();
  const [anchor, setAnchor] = useState<null | HTMLElement>(null);
  if (!enabled) {
    return (
      <Typography variant="caption" color="text.secondary">
        auth disabled (dev)
      </Typography>
    );
  }
  const name =
    (profile?.preferred_username as string) ||
    (profile?.name as string) ||
    (profile?.email as string) ||
    "account";
  return (
    <>
      <Button
        color="inherit"
        size="small"
        startIcon={<AccountCircleIcon />}
        onClick={(e) => setAnchor(e.currentTarget)}
      >
        {name}
      </Button>
      <Menu anchorEl={anchor} open={!!anchor} onClose={() => setAnchor(null)}>
        <MenuItem
          onClick={() => {
            setAnchor(null);
            logout();
          }}
        >
          Sign out
        </MenuItem>
      </Menu>
    </>
  );
}

export function Layout({ children }: { children: ReactNode }) {
  const location = useLocation();
  const { can } = usePermissions();
  const nav = can("iam:read")
    ? [...NAV, { to: "/iam", label: "Access control", icon: <SecurityIcon /> }]
    : NAV;
  const isActive = (to: string) =>
    to === "/" ? location.pathname === "/" : location.pathname.startsWith(to);

  return (
    <Box sx={{ display: "flex" }}>
      <AppBar position="fixed" sx={{ zIndex: (t) => t.zIndex.drawer + 1 }} color="default">
        <Toolbar variant="dense">
          <ComputerIcon sx={{ mr: 1 }} color="primary" />
          <Typography variant="h6" noWrap>
            ch-orchestrator
          </Typography>
          <Box sx={{ flexGrow: 1 }} />
          <UserMenu />
        </Toolbar>
      </AppBar>
      <Drawer
        variant="permanent"
        sx={{
          width: DRAWER_WIDTH,
          flexShrink: 0,
          [`& .MuiDrawer-paper`]: { width: DRAWER_WIDTH, boxSizing: "border-box" },
        }}
      >
        <Toolbar variant="dense" />
        <List>
          {nav.map((item) => (
            <ListItemButton
              key={item.to}
              component={RouterLink}
              to={item.to}
              selected={isActive(item.to)}
            >
              <ListItemIcon sx={{ minWidth: 40 }}>{item.icon}</ListItemIcon>
              <ListItemText primary={item.label} />
            </ListItemButton>
          ))}
        </List>
      </Drawer>
      <Box component="main" sx={{ flexGrow: 1, p: 3, mt: 6, minWidth: 0 }}>
        {children}
      </Box>
    </Box>
  );
}
