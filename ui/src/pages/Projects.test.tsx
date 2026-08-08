// What matters on this page is that a quota is legible and that "blank means
// unlimited" survives the round trip — the rest is a list.

import { describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Projects } from "./Projects";
import { renderWithProviders } from "../test/render";
import { stubFetch } from "../test/setup";

const DEFAULT_ID = "00000000-0000-0000-0000-000000000001";
const BLUE_ID = "11111111-1111-1111-1111-111111111111";

const PROJECTS = [
  { id: DEFAULT_ID, name: "default", is_default: true, created_at: "", updated_at: "" },
  { id: BLUE_ID, name: "team-blue", is_default: false, created_at: "", updated_at: "" },
];

const ME = {
  authenticated: true,
  username: "alice",
  permissions: ["project:read", "project:create", "project:update", "project:delete"],
  project: DEFAULT_ID,
  tenancy: true,
  platform: true,
};

function quota(over: Record<string, unknown> = {}) {
  return {
    limits: {
      max_vms: 4,
      max_vcpus: null,
      max_memory_mib: null,
      max_volumes: null,
      max_storage_bytes: null,
    },
    usage: { vms: 2, vcpus: 6, memory_mib: 4096, volumes: 1, storage_bytes: 1024 },
    over_quota: false,
    ...over,
  };
}

describe("Projects", () => {
  it("shows usage against its limit, and names the unlimited dimensions", async () => {
    stubFetch({ "/me": ME, "/projects": PROJECTS, [`/projects/${DEFAULT_ID}/quota`]: quota() });
    renderWithProviders(<Projects />);

    // "2 / 4" for the capped dimension; the rest say so rather than showing 0.
    expect(await screen.findByText(/2\s*\/\s*4/)).toBeInTheDocument();
    expect(screen.getAllByText(/unlimited/).length).toBeGreaterThan(0);
  });

  /// Being over quota is a legitimate state — it is what lowering a limit does —
  /// so the page has to explain it rather than look broken.
  it("says so when usage is past a limit, without implying data was lost", async () => {
    stubFetch({
      "/me": ME,
      "/projects": PROJECTS,
      [`/projects/${DEFAULT_ID}/quota`]: quota({ over_quota: true }),
    });
    renderWithProviders(<Projects />);
    expect(await screen.findByText("Over quota")).toBeInTheDocument();
    expect(screen.getByText(/Nothing has been deleted/)).toBeInTheDocument();
  });

  /// The round trip that matters: a field left blank has to reach the API as
  /// `null`, because the API treats a whole-object write as authoritative and
  /// anything else would silently keep a limit the operator just cleared.
  it("sends a cleared field as null, not as a missing key", async () => {
    const bodies: unknown[] = [];
    stubFetch({
      "/me": ME,
      "/projects": PROJECTS,
      [`/projects/${DEFAULT_ID}/quota`]: (init: RequestInit | undefined) => {
        if (init?.method === "PUT") bodies.push(JSON.parse(init.body as string));
        return quota();
      },
    });
    renderWithProviders(<Projects />);

    await userEvent.click(await screen.findByRole("button", { name: /Edit quota/ }));
    const vms = await screen.findByDisplayValue("4");
    await userEvent.clear(vms);
    await userEvent.click(screen.getByRole("button", { name: "Save quota" }));

    await waitFor(() => expect(bodies).toHaveLength(1));
    expect(bodies[0]).toEqual({
      max_vms: null,
      max_vcpus: null,
      max_memory_mib: null,
      max_volumes: null,
      max_storage_bytes: null,
    });
  });

  it("refuses to submit a limit that is not a whole number", async () => {
    stubFetch({ "/me": ME, "/projects": PROJECTS, [`/projects/${DEFAULT_ID}/quota`]: quota() });
    renderWithProviders(<Projects />);

    await userEvent.click(await screen.findByRole("button", { name: /Edit quota/ }));
    const vms = await screen.findByDisplayValue("4");
    await userEvent.clear(vms);
    await userEvent.type(vms, "3.5");
    expect(await screen.findByText(/whole numbers/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save quota" })).toBeDisabled();
  });

  /// Deleting the default project is refused by the control plane; not offering
  /// the button is how the console stops an operator finding that out the hard
  /// way.
  it("does not offer to delete the default project", async () => {
    stubFetch({ "/me": ME, "/projects": PROJECTS, [`/projects/${DEFAULT_ID}/quota`]: quota() });
    renderWithProviders(<Projects />);
    await screen.findByText(/every caller without a project context/);
    expect(screen.queryByRole("button", { name: "Delete project" })).not.toBeInTheDocument();
  });

  it("hides every mutating action from a caller who only reads", async () => {
    stubFetch({
      "/me": { ...ME, permissions: ["project:read"] },
      "/projects": PROJECTS,
      [`/projects/${DEFAULT_ID}/quota`]: quota(),
    });
    renderWithProviders(<Projects />);
    await screen.findByText("Quota");
    expect(screen.queryByRole("button", { name: "Create project" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /quota/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
  });
});
