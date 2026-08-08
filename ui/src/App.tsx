import { Suspense, lazy } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Overview } from "./pages/Overview";
import { Hosts } from "./pages/Hosts";
import { HostDetail } from "./pages/HostDetail";
import { Vms } from "./pages/Vms";
import { CreateVm } from "./pages/CreateVm";
import { CreateVmFromTemplate } from "./pages/CreateVmFromTemplate";
import { VmDetail } from "./pages/VmDetail";
import { Networks } from "./pages/Networks";
import { SecurityGroups } from "./pages/SecurityGroups";
import { Images } from "./pages/Images";
import { Volumes } from "./pages/Volumes";
import { Templates } from "./pages/Templates";
import { Tasks } from "./pages/Tasks";
import { Events } from "./pages/Events";
import { Iam } from "./pages/Iam";
import { Projects } from "./pages/Projects";
import { Settings } from "./pages/Settings";
// xterm is ~290 kB and only the serial console needs it. Splitting it out keeps
// it off the critical path for every operator who never opens a console.
const Console = lazy(() =>
  import("./pages/Console").then((m) => ({ default: m.Console })),
);
import { useAuth } from "./auth/AuthProvider";
import { usePermissions } from "./auth/permissions";
import { ACTION, READ } from "./auth/perm";
import type { Permission } from "./auth/perm";
import { Login } from "./pages/Login";
import { EmptyState, SkeletonRows } from "./ui/kit";

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

/// Hiding a nav item is not access control. The control plane rejects the
/// request either way, but a route the caller cannot read should not render its
/// screen and fire its queries.
function Require({ perm, children }: { perm: Permission; children: React.ReactNode }) {
  const { can, loading } = usePermissions();
  if (loading) {
    return (
      <div className="vq-table">
        <SkeletonRows cols="1.5fr 1fr 1fr" rows={4} />
      </div>
    );
  }
  if (!can(perm)) {
    return (
      <EmptyState
        headline="You do not have access to this page"
        hint="Ask an administrator for the permission it requires."
      />
    );
  }
  return <>{children}</>;
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
        <Route
          path="/vms/:id/console"
          element={
            <Suspense fallback={<div className="vq-skel" style={{ height: 460 }} />}>
              <Console />
            </Suspense>
          }
        />
        <Route path="/networks" element={<Networks />} />
        <Route path="/security-groups" element={<SecurityGroups />} />
        <Route path="/images" element={<Images />} />
        <Route path="/volumes" element={<Volumes />} />
        <Route path="/templates" element={<Templates />} />
        <Route path="/templates/:id/launch" element={<CreateVmFromTemplate />} />
        <Route path="/tasks" element={<Tasks />} />
        <Route path="/events" element={<Events />} />
        <Route
          path="/projects"
          element={
            <Require perm={READ.projects}>
              <Projects />
            </Require>
          }
        />
        <Route
          path="/iam"
          element={
            <Require perm={ACTION.iamRead}>
              <Iam />
            </Require>
          }
        />
        <Route path="/settings" element={<Settings />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}
