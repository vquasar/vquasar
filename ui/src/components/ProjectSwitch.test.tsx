// The switcher decides what every other screen is showing, so these tests are
// about the consequences of a selection rather than about the menu widget.
//
// Nothing is mocked but `fetch`. The provider, the query client and the
// component are all real, so what these assert is the request that would go on
// the wire.

import { describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectSwitch } from "./ProjectSwitch";
import { renderWithProviders } from "../test/render";
import { stubFetch } from "../test/setup";

const DEFAULT_ID = "00000000-0000-0000-0000-000000000001";
const BLUE_ID = "11111111-1111-1111-1111-111111111111";

const PROJECTS = [
  { id: DEFAULT_ID, name: "default", is_default: true, created_at: "", updated_at: "" },
  { id: BLUE_ID, name: "team-blue", is_default: false, created_at: "", updated_at: "" },
];

function me(over: Record<string, unknown> = {}) {
  return {
    authenticated: true,
    username: "alice",
    permissions: ["project:read", "vm:read"],
    project: DEFAULT_ID,
    tenancy: true,
    platform: true,
    ...over,
  };
}

describe("ProjectSwitch", () => {
  it("renders nothing when tenancy is off", async () => {
    stubFetch({ "/me": me({ tenancy: false, project: undefined }), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    // Wait for /me to resolve, so this is "stayed hidden" rather than
    // "had not rendered yet".
    await waitFor(() => expect(screen.queryByText("Project")).not.toBeInTheDocument());
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("shows the caller's default project when nothing is selected", async () => {
    stubFetch({ "/me": me(), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    expect(await screen.findByText("default")).toBeInTheDocument();
  });

  it("sends the chosen project on subsequent requests", async () => {
    const calls = stubFetch({ "/me": me(), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    await screen.findByText("default");

    await userEvent.click(screen.getByRole("button"));
    await userEvent.click(await screen.findByRole("menuitem", { name: "team-blue" }));

    expect(await screen.findByText("team-blue")).toBeInTheDocument();
    // Switching invalidates every query, so /me refetches — under the new
    // header. That refetch is the proof the selection reached the client.
    await waitFor(() => {
      const meCalls = calls.filter((c) => c.url.includes("/me"));
      expect(meCalls.at(-1)?.project).toBe(BLUE_ID);
    });
  });

  it("sends no header at all while tenancy is off", async () => {
    const calls = stubFetch({
      "/me": me({ tenancy: false, project: undefined }),
      "/projects": PROJECTS,
    });
    renderWithProviders(<ProjectSwitch />);
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    expect(calls.every((c) => c.project === null)).toBe(true);
  });

  it("offers the platform view only to a caller who holds a platform binding", async () => {
    stubFetch({ "/me": me({ platform: false }), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    await screen.findByText("default");
    await userEvent.click(screen.getByRole("button"));
    expect(await screen.findByRole("menuitem", { name: "team-blue" })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "All projects" })).not.toBeInTheDocument();
  });

  it("offers the platform view when the caller does hold one", async () => {
    stubFetch({ "/me": me(), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    await screen.findByText("default");
    await userEvent.click(screen.getByRole("button"));
    expect(await screen.findByRole("menuitem", { name: "All projects" })).toBeInTheDocument();
  });

  it("restores a stored selection across a reload", async () => {
    localStorage.setItem("vquasar.project", BLUE_ID);
    const calls = stubFetch({ "/me": me(), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    expect(await screen.findByText("team-blue")).toBeInTheDocument();
    expect(calls.every((c) => c.project === BLUE_ID)).toBe(true);
  });

  /// The case that makes persistence safe: a tab left open (or reopened) after
  /// the binding was revoked must not stay pinned to a project every request
  /// will now be refused from.
  it("drops a stored selection the caller can no longer act in", async () => {
    localStorage.setItem("vquasar.project", BLUE_ID);
    const calls = stubFetch({
      "/me": me(),
      // team-blue is gone from what this caller can see.
      "/projects": [PROJECTS[0]],
    });
    renderWithProviders(<ProjectSwitch />);
    expect(await screen.findByText("default")).toBeInTheDocument();
    await waitFor(() => expect(localStorage.getItem("vquasar.project")).toBeNull());
    expect(calls.at(-1)?.project).toBeNull();
  });

  /// Same, for the platform view: `*` is not a privilege, and a caller who has
  /// lost their platform binding would otherwise sit in a view where every
  /// request resolves to no permissions.
  it("drops a stored platform view when the caller may no longer take it", async () => {
    localStorage.setItem("vquasar.project", "*");
    stubFetch({ "/me": me({ platform: false }), "/projects": PROJECTS });
    renderWithProviders(<ProjectSwitch />);
    expect(await screen.findByText("default")).toBeInTheDocument();
    await waitFor(() => expect(localStorage.getItem("vquasar.project")).toBeNull());
  });
});
