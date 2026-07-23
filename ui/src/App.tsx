import { Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Dashboard } from "./pages/Dashboard";
import { Hosts } from "./pages/Hosts";
import { Vms } from "./pages/Vms";
import { CreateVm } from "./pages/CreateVm";
import { VmDetail } from "./pages/VmDetail";
import { Networks } from "./pages/Networks";
import { Tasks } from "./pages/Tasks";
import { Events } from "./pages/Events";

export function App() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<Dashboard />} />
        <Route path="/hosts" element={<Hosts />} />
        <Route path="/vms" element={<Vms />} />
        <Route path="/vms/new" element={<CreateVm />} />
        <Route path="/vms/:id" element={<VmDetail />} />
        <Route path="/networks" element={<Networks />} />
        <Route path="/tasks" element={<Tasks />} />
        <Route path="/events" element={<Events />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}
