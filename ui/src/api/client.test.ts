// The client is where the project and the bearer token get attached to every
// request. Both are registered getters rather than call-site arguments, which
// is the whole point — so what is worth testing is that a request carries them
// without anyone having remembered to pass them.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { api, setProjectGetter, setTokenGetter, ApiError } from "./client";

function captureHeaders() {
  const seen: Record<string, string>[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      seen.push((init?.headers ?? {}) as Record<string, string>);
      return new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }),
  );
  return seen;
}

describe("api client", () => {
  beforeEach(() => {
    setProjectGetter(() => null);
    setTokenGetter(() => null);
  });

  it("sends no project header when none is selected", async () => {
    const seen = captureHeaders();
    await api.get("/vms");
    expect(seen[0]["x-vquasar-project"]).toBeUndefined();
  });

  it("attaches the selected project to every verb", async () => {
    setProjectGetter(() => "team-blue");
    const seen = captureHeaders();
    await api.get("/vms");
    await api.post("/vms", { name: "x" });
    await api.patch("/vms/1", {});
    await api.put("/projects/1/quota", {});
    await api.del("/vms/1");
    expect(seen).toHaveLength(5);
    expect(seen.every((h) => h["x-vquasar-project"] === "team-blue")).toBe(true);
  });

  /// `*` is the platform view. It is a header value, not an absence — sending
  /// nothing would mean the caller's default project, which is a different
  /// request entirely.
  it("treats the platform view as a value, not an absence", async () => {
    setProjectGetter(() => "*");
    const seen = captureHeaders();
    await api.get("/vms");
    expect(seen[0]["x-vquasar-project"]).toBe("*");
  });

  it("carries the bearer token alongside it", async () => {
    setProjectGetter(() => "team-blue");
    setTokenGetter(() => "tok123");
    const seen = captureHeaders();
    await api.get("/vms");
    expect(seen[0].authorization).toBe("Bearer tok123");
    expect(seen[0]["x-vquasar-project"]).toBe("team-blue");
  });

  /// The error envelope is the contract the console renders from (design §37),
  /// and a quota refusal is the case where the message carries the arithmetic
  /// an operator needs — dropping it for a bare status would be a regression.
  it("surfaces the error envelope's code and message", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              error: {
                code: "QUOTA_EXCEEDED",
                message: "project quota exceeded: vms — limit 2, 2 in use, this request asks for 1",
                request_id: "abc",
              },
            }),
            { status: 409, headers: { "content-type": "application/json" } },
          ),
      ),
    );
    await expect(api.post("/vms", {})).rejects.toThrow(ApiError);
    await expect(api.post("/vms", {})).rejects.toMatchObject({
      code: "QUOTA_EXCEEDED",
      requestId: "abc",
    });
  });
});
