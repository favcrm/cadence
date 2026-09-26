import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource/ibm-plex-sans/latin-400.css";
import "@fontsource/ibm-plex-sans/latin-500.css";
import "@fontsource/ibm-plex-sans/latin-600.css";
import "@fontsource/ibm-plex-mono/latin-400.css";
import "@fontsource/ibm-plex-mono/latin-500.css";
import "@fontsource/ibm-plex-mono/latin-600.css";
import "./styles.css";
import App from "./App";
import { applyLegacyRedirect, installClientNav } from "./lib/useLocation";
import { initTheme } from "./lib/theme";
import { captureLoginNonce } from "./features/auth/session";

// The stored theme pick, before the first paint of the app.
initTheme();

// A pre-router link (`/?tab=board&project=cadence`) opens at its route.
applyLegacyRedirect();

// Same-origin links stay in the SPA (CAD-609). A full load would drop
// the wiki tree's expanded folders.
installClientNav();

// A `cadence ui login` link: take its nonce out of the address bar
// before anything renders (CAD-313).
captureLoginNonce();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
