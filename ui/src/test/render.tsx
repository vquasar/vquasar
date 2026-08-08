// Render a component inside the providers the real app wraps it in.
//
// A component test that skips the providers is testing something the console
// never runs. Each render gets its own QueryClient so one test's cache cannot
// answer another test's query.

import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { render } from "@testing-library/react";
import { ProjectProvider } from "../auth/ProjectProvider";

export function renderWithProviders(ui: ReactNode, { route = "/" } = {}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        // A test asserting a failure should see it immediately, not after the
        // retry budget; and background refetching would make assertions racy.
        retry: false,
        refetchOnWindowFocus: false,
        refetchInterval: false,
      },
    },
  });
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[route]}>
          <ProjectProvider>{ui}</ProjectProvider>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  };
}
