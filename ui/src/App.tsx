import { Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Overview } from "./pages/Overview";
import { Hosts } from "./pages/Hosts";
import { HostDetail } from "./pages/HostDetail";
import { Vms } from "./pages/Vms";
import { CreateVm } from "./pages/CreateVm";
import { CreateVmFromTemplate } from "./pages/CreateVmFromTemplate";
import { VmDetail } from "./pages/VmDetail";
import { Console } from "./pages/Console";
import { Networks } from "./pages/Networks";
import { SecurityGroups } from "./pages/SecurityGroups";
import { Images } from "./pages/Images";
import { Volumes } from "./pages/Volumes";
import { Templates } from "./pages/Templates";
import { Tasks } from "./pages/Tasks";
import { Events } from "./pages/Events";
import { Iam } from "./pages/Iam";
import { Settings } from "./pages/Settings";
import { useAuth } from "./auth/AuthProvider";
import { Login } from "./pages/Login";
import { SkeletonRows } from "./ui/kit";

export function App() {
  const { loading, authenticated } = useAuth();

  // Skeleton the shell rather than blocking the whole page on a spinner.
  if (loading) {
    return (
      <div className="vq-app">
        <div className="vq-sidebar" />
        <div className="vq-body">
          <div className="vq-topbar" />
          <main className="vq-main">
            <div className="vq-table">
              <SkeletonRows cols="1.5fr 1fr 1fr 1fr" rows={6} />
            </div>
          </main>
        </div>
      </div>
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
        <Route path="/" element={<Overview />} />
        <Route path="/hosts" element={<Hosts />} />
        <Route path="/hosts/:id" element={<HostDetail />} />
        <Route path="/vms" element={<Vms />} />
        <Route path="/vms/new" element={<CreateVm />} />
        <Route path="/vms/:id" element={<VmDetail />} />
        <Route path="/vms/:id/console" element={<Console />} />
        <Route path="/networks" element={<Networks />} />
        <Route path="/security-groups" element={<SecurityGroups />} />
        <Route path="/images" element={<Images />} />
        <Route path="/volumes" element={<Volumes />} />
        <Route path="/templates" element={<Templates />} />
        <Route path="/templates/:id/launch" element={<CreateVmFromTemplate />} />
        <Route path="/tasks" element={<Tasks />} />
        <Route path="/events" element={<Events />} />
        <Route path="/iam" element={<Iam />} />
        <Route path="/settings" element={<Settings />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}
