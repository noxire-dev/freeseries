import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockEngine, mockHotkeys, mockInfo, mockLog, mockSetup, mockStartup, mockWindow, tauriEngine, tauriHotkeys, tauriInfo, tauriLog, tauriSetup, tauriStartup, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win, log, startup, setup, info, hotkeys] = inTauri
  ? await Promise.all([tauriEngine(), tauriWindow(), tauriLog(), tauriStartup(), tauriSetup(), tauriInfo(), tauriHotkeys()])
  : [mockEngine(), mockWindow(), mockLog(), mockStartup(), mockSetup(), mockInfo(), mockHotkeys()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} log={log} startup={startup} setup={setup} info={info} hotkeys={hotkeys} autoStart={!(inTauri && import.meta.env.DEV)} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
