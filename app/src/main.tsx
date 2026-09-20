import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockEngine, mockLog, mockWindow, tauriEngine, tauriLog, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win, log] = inTauri ? await Promise.all([tauriEngine(), tauriWindow(), tauriLog()]) : [mockEngine(), mockWindow(), mockLog()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} log={log} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
