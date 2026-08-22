import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

const ZOOM_KEY = "biturbo.uiZoom";

/** WebKitGTK (WSL) ignores CSS `zoom`; scale #root via CSS variables instead. */
function applyUiZoom(zoom: number) {
  const html = document.documentElement;
  if (!Number.isFinite(zoom) || zoom <= 0) return;

  if (zoom === 1) {
    html.style.removeProperty("--biturbo-ui-zoom");
    delete html.dataset.uiZoom;
    return;
  }

  html.style.setProperty("--biturbo-ui-zoom", String(zoom));
  html.dataset.uiZoom = String(zoom);
}

const fromEnv = import.meta.env.VITE_UI_ZOOM;
if (fromEnv != null && String(fromEnv).length > 0) {
  try {
    localStorage.setItem(ZOOM_KEY, String(fromEnv));
  } catch {
    /* ignore quota / private mode */
  }
}

const raw = localStorage.getItem(ZOOM_KEY) ?? fromEnv;
const uiZoom = Number(raw);
if (Number.isFinite(uiZoom) && uiZoom > 0) {
  applyUiZoom(uiZoom);
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
