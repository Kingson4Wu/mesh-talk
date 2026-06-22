import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
// Bundle fonts LOCALLY (offline desktop app, CSP-safe — no CDN). Variable packages.
import "@fontsource-variable/space-grotesk"; // display
import "@fontsource-variable/inter"; // body
import "@fontsource-variable/geist-mono"; // mono (crypto/identity/ports/IDs)
import "./lib/theme"; // apply the persisted theme before first paint
import "./lib/i18n"; // initialize i18next before first paint
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
