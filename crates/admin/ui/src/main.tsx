import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { ApiError } from "./api";
import { LOGIN_URL, navigate } from "./router";
import "./styles.css";

const client = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 2,
    },
  },
});

// A 401 from any query means the session ended: the session query decides where to go.
client.getQueryCache().subscribe((event) => {
  const error = event.type === "updated" ? event.action.type === "error" && event.action.error : null;
  if (error instanceof ApiError && error.status === 401 && event.query.queryKey[0] !== "session") {
    client.setQueryData(["session"], { authenticated: false, user: null });
    navigate(LOGIN_URL, true);
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
