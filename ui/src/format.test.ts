import { describe, expect, it } from "vitest";
import { fleetSummary, fleetVersions } from "./format";

describe("fleetVersions", () => {
  it("collapses agreement to one version", () => {
    const f = fleetVersions(["v1", "v1", "v1"]);
    expect(f.known).toEqual(["v1"]);
    expect(f.unknown).toBe(0);
    expect(fleetSummary(f, "agent")).toBe("agent v1");
  });

  it("counts a host that reported nothing instead of dropping it", () => {
    // The case this exists for. Filtering the nulls away leaves ["v1"], which
    // renders as "everyone is on v1" — a claim about two hosts that only one
    // of them made.
    const f = fleetVersions(["v1", null, undefined]);
    expect(f.known).toEqual(["v1"]);
    expect(f.unknown).toBe(2);
    expect(fleetSummary(f, "agent")).toBe("agent v1, 2 unknown");
  });

  it("says how many versions there are when they disagree", () => {
    const f = fleetVersions(["v2", "v1"]);
    // Sorted, so the summary does not change with host order.
    expect(f.known).toEqual(["v1", "v2"]);
    expect(fleetSummary(f, "agent")).toBe("2 agent versions");
  });

  it("reports unknown when nobody has said", () => {
    expect(fleetSummary(fleetVersions([null, null]), "agent")).toBe("agent unknown");
    expect(fleetSummary(fleetVersions([]), "cloud-hypervisor")).toBe("cloud-hypervisor unknown");
  });

  it("does not treat an empty string as a version", () => {
    // A proto3 string field is empty, never absent, so "no build reported"
    // arrives as "" — it must not become a version everyone appears to share.
    const f = fleetVersions(["", "", "v1"]);
    expect(f.known).toEqual(["v1"]);
    expect(f.unknown).toBe(2);
  });
});
