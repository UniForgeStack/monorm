import * as React from "react";
import { StrictMode } from "react";
import * as ReactDOM from "react-dom";
import * as ReactDOMClient from "react-dom/client";
import * as jsxRuntime from "react/jsx-runtime";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { ApiError } from "./api";
import * as ui from "./kit";
import { initTheme } from "./lib/theme";
import "./styles.css";

// The one React of the page: the import map of a site with packages points `import "react"` at the modules of <base>/shared, which read this.
(globalThis as Record<string, unknown>).__monormAdmin = { React, ReactDOM, ReactDOMClient, jsxRuntime, ui };

initTheme();

const client = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 2,
    },
  },
});

// A 401 from any query means the session ended: the session query is read again and the app decides where to go (the form, or the provider when the site starts the sign-in at once).
client.getQueryCache().subscribe((event) => {
  const error = event.type === "updated" ? event.action.type === "error" && event.action.error : null;
  if (error instanceof ApiError && error.status === 401 && event.query.queryKey[0] !== "session") {
    void client.invalidateQueries({ queryKey: ["session"] });
  }
});

const root = document.getElementById("root");
if (!root) throw new Error("no #root");
createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
