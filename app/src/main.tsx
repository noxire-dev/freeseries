import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockEngine, mockLog, mockStartup, mockWindow, tauriEngine, tauriLog, tauriStartup, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win, log, startup] = inTauri
  ? await Promise.all([tauriEngine(), tauriWindow(), tauriLog(), tauriStartup()])
  : [mockEngine(), mockWindow(), mockLog(), mockStartup()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} log={log} startup={startup} autoStart={!(inTauri && import.meta.env.DEV)} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
