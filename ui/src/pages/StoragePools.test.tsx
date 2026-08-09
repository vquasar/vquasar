// What matters on this page is that a pool nobody reports does not read as
// fine, and that opening it says *why* — the two things a row of configuration
// cannot tell you on its own (ADR-023).

import { describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StoragePools } from "./StoragePools";
import { renderWithProviders } from "../test/render";
import { stubFetch } from "../test/setup";

const READY_ID = "11111111-1111-4111-8111-111111111111";
const PENDING_ID = "22222222-2222-4222-8222-222222222222";

const ME = {
  authenticated: true,
  username: "alice",
  permissions: ["storagepool:read", "storagepool:manage"],
  project: "00000000-0000-0000-0000-000000000001",
  tenancy: false,
  platform: true,
};

function pool(over: Record<string, unknown> = {}) {
  return {
    id: READY_ID,
    name: "default",
    description: null,
    kind: "shared_dir",
    params: { kind: "shared_dir", path: "/var/lib/vquasar/shared/volumes" },
    state: "ready",
    reachable_hosts: 2,
    capacity_bytes: 1024 * 1024 * 1024 * 100,
    available_bytes: 1024 * 1024 * 1024 * 40,
    created_at: "",
    updated_at: "",
    ...over,
  };
}

const PENDING = pool({
  id: PENDING_ID,
  name: "fast",
  params: { kind: "shared_dir", path: "/srv/fast" },
  state: "pending",
  reachable_hosts: 0,
  capacity_bytes: null,
  available_bytes: null,
});

describe("StoragePools", () => {
  it("shows a pool nobody reports as pending, with no size it cannot know", async () => {
    stubFetch({ "/me": ME, "/storage-pools": [pool(), PENDING] });
    renderWithProviders(<StoragePools />);

    expect(await screen.findByText("fast")).toBeInTheDocument();
    expect(screen.getByText("pending")).toBeInTheDocument();
    expect(screen.getByText("ready")).toBeInTheDocument();
    // A pool no host reports has no capacity to report either. Showing 0 would
    // read as "full" rather than "unknown".
    expect(screen.queryByText(/0 B \//)).not.toBeInTheDocument();
  });

  /// The question after a refused placement is never *whether* a host can see
  /// the storage, it is why not. That answer has to be one click away.
  it("names the host that refused a pool, and its reason", async () => {
    stubFetch({
      "/me": ME,
      "/storage-pools": [PENDING],
      [`/storage-pools/${PENDING_ID}`]: {
        ...PENDING,
        hosts: [
          {
            host_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            host_name: "hv-3",
            usable: false,
            message: "/srv/fast does not exist here — the pool is probably not mounted",
            capacity_bytes: null,
            available_bytes: null,
            reported_at: "2026-08-09T00:00:00Z",
          },
        ],
      },
    });
    renderWithProviders(<StoragePools />);

    await userEvent.click(await screen.findByText("fast"));
    expect(await screen.findByText("hv-3")).toBeInTheDocument();
    expect(screen.getByText(/not mounted/)).toBeInTheDocument();
  });

  /// A pool with no reports at all is the common case on a fresh cluster, and
  /// an empty table would look like a bug rather than a mount that has not
  /// come back.
  it("explains an empty report list instead of showing nothing", async () => {
    stubFetch({
      "/me": ME,
      "/storage-pools": [PENDING],
      [`/storage-pools/${PENDING_ID}`]: { ...PENDING, hosts: [] },
    });
    renderWithProviders(<StoragePools />);

    await userEvent.click(await screen.findByText("fast"));
    expect(await screen.findByText(/No host has reported this pool/)).toBeInTheDocument();
  });

  it("offers no way to add a pool without the permission to", async () => {
    stubFetch({
      "/me": { ...ME, permissions: ["storagepool:read"] },
      "/storage-pools": [pool()],
    });
    renderWithProviders(<StoragePools />);

    expect(await screen.findByText("default")).toBeInTheDocument();
    expect(screen.queryByText("Add pool")).not.toBeInTheDocument();
  });
});
