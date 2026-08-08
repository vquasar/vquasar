// Test environment for the console's component tests.
//
// The rule these tests follow: stub the network and nothing else. React, React
// Query, MUI and the components under test are all real, so a passing test
// exercises the same code path a browser does right up to the wire. Mocking a
// hook to make an assertion pass proves the mock works, not the console.

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeEach, vi } from "vitest";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

// jsdom has no localStorage quota and no cross-test isolation, so clear it
// between tests: the project selection persists there deliberately, and one
// test's selection leaking into the next would hide exactly the bug that
// persistence can cause.
beforeEach(() => {
  localStorage.clear();
});

/// Route table for a stubbed `fetch`: path (without the `/api/v1` prefix) to
/// the JSON body it answers with. A function receives the `RequestInit` so a
/// test can assert on headers or return different bodies per project.
export type Routes = Record<
  string,
  unknown | ((init: RequestInit | undefined, url: string) => unknown)
>;

export interface FetchRecord {
  url: string;
  project: string | null;
}

/// Install a `fetch` that answers from `routes` and records what it was asked.
///
/// An unrouted path is a 404 rather than a hang or an empty object: a test that
/// forgets to route something should fail loudly, in that test, instead of
/// leaving a component in a permanent loading state that times out somewhere
/// unrelated.
export function stubFetch(routes: Routes): FetchRecord[] {
  const calls: FetchRecord[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const headers = (init?.headers ?? {}) as Record<string, string>;
      calls.push({ url, project: headers["x-vquasar-project"] ?? null });

      const path = url.replace(/^.*\/api\/v1/, "").replace(/\?.*$/, "");
      if (!(path in routes)) {
        return new Response(
          JSON.stringify({
            error: { code: "NOT_FOUND", message: `no stub for ${path}`, request_id: "test" },
          }),
          { status: 404, headers: { "content-type": "application/json" } },
        );
      }
      const entry = routes[path];
      const body = typeof entry === "function" ? entry(init, url) : entry;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }),
  );
  return calls;
}
