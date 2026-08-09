// What matters here is that the palette is predictable and does not show a
// caller things they cannot read. The rest is a list.

import { describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { CommandPalette } from "./CommandPalette";
import { renderWithProviders } from "../test/render";
import { stubFetch } from "../test/setup";

const DEFAULT_ID = "00000000-0000-0000-0000-000000000001";

function me(permissions: string[]) {
  return {
    authenticated: true,
    username: "alice",
    permissions,
    project: DEFAULT_ID,
    tenancy: false,
    platform: true,
  };
}

const VMS = [
  { id: "11111111-1111-4111-8111-111111111111", name: "web-01", phase: "Running" },
  { id: "22222222-2222-4222-8222-222222222222", name: "db-01", phase: "Running" },
];
// Deliberately shorter than the VM's name: if ranking ignored *where* the
// match falls and sorted by length alone, this would come first.
const HOSTS = [{ id: "33333333-3333-4333-8333-333333333333", name: "aweb", state: "Ready" }];

describe("CommandPalette", () => {
  it("shows pages before anything is typed, not every VM in the fleet", async () => {
    stubFetch({ "/me": me(["vm:read", "host:read"]), "/vms": VMS, "/hosts": HOSTS });
    renderWithProviders(<CommandPalette open onOpenChange={() => {}} />);

    expect(await screen.findByText("Virtual machines")).toBeInTheDocument();
    // A palette that opens onto a hundred VMs is a list, not a shortcut.
    expect(screen.queryByText("web-01")).not.toBeInTheDocument();
  });

  /// The ranking is the whole reason to prefer substring over fuzzy: typing
  /// the same letters has to land in the same place every time.
  it("ranks a name that starts with the query above one that merely contains it", async () => {
    stubFetch({
      "/me": me(["vm:read", "host:read"]),
      "/vms": VMS,
      "/hosts": HOSTS,
    });
    renderWithProviders(<CommandPalette open onOpenChange={() => {}} />);

    await userEvent.type(await screen.findByLabelText("Search"), "web");
    const rows = screen.getAllByRole("button").map((b) => b.textContent ?? "");
    const vm = rows.findIndex((r) => r.includes("web-01"));
    const host = rows.findIndex((r) => r.includes("aweb"));
    expect(vm).toBeGreaterThanOrEqual(0);
    expect(host).toBeGreaterThanOrEqual(0);
    expect(vm).toBeLessThan(host);
  });

  /// The palette searches what the console has loaded, and a caller without
  /// `vm:read` never loaded any VMs — so there is nothing to filter out after
  /// the fact, which is how this kind of thing becomes an enumeration oracle.
  it("cannot surface a resource the caller may not read", async () => {
    stubFetch({ "/me": me(["host:read"]), "/hosts": HOSTS });
    renderWithProviders(<CommandPalette open onOpenChange={() => {}} />);

    // The page list itself is gated, which only shows before a query narrows
    // it — asserting this after typing would pass for the wrong reason.
    expect(await screen.findByText("Hosts")).toBeInTheDocument();
    expect(screen.queryByText("Virtual machines")).not.toBeInTheDocument();

    await userEvent.type(await screen.findByLabelText("Search"), "web");
    expect(await screen.findByText("aweb")).toBeInTheDocument();
    expect(screen.queryByText("web-01")).not.toBeInTheDocument();
  });

  it("says so when nothing matches, rather than showing an empty box", async () => {
    stubFetch({ "/me": me(["vm:read"]), "/vms": VMS });
    renderWithProviders(<CommandPalette open onOpenChange={() => {}} />);

    await userEvent.type(await screen.findByLabelText("Search"), "zzzz");
    expect(await screen.findByText(/Nothing matches/)).toBeInTheDocument();
  });
});
