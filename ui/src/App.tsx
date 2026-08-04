import { Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Dashboard } from "./pages/Dashboard";
import { Hosts } from "./pages/Hosts";
import { Vms } from "./pages/Vms";
import { CreateVm } from "./pages/CreateVm";
import { VmDetail } from "./pages/VmDetail";
import { Console } from "./pages/Console";
import { Networks } from "./pages/Networks";
import { Images } from "./pages/Images";
import { Templates } from "./pages/Templates";
import { Tasks } from "./pages/Tasks";
import { Events } from "./pages/Events";
import { Iam } from "./pages/Iam";
import { useAuth } from "./auth/AuthProvider";
import { Login } from "./pages/Login";
import Box from "@mui/material/Box";
import CircularProgress from "@mui/material/CircularProgress";

export function App() {
  const { loading, authenticated } = useAuth();

  if (loading) {
    return (
      <Box sx={{ display: "flex", justifyContent: "center", alignItems: "center", minHeight: "100vh" }}>
        <CircularProgress />
      </Box>
    );
  }
  if (!authenticated) {
    return <Login />;
  }

  return <AuthedApp />;
}

function AuthedApp() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<Dashboard />} />
        <Route path="/hosts" element={<Hosts />} />
        <Route path="/vms" element={<Vms />} />
        <Route path="/vms/new" element={<CreateVm />} />
        <Route path="/vms/:id" element={<VmDetail />} />
        <Route path="/vms/:id/console" element={<Console />} />
        <Route path="/networks" element={<Networks />} />
        <Route path="/images" element={<Images />} />
        <Route path="/templates" element={<Templates />} />
        <Route path="/tasks" element={<Tasks />} />
        <Route path="/events" element={<Events />} />
        <Route path="/iam" element={<Iam />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}
