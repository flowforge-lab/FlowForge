import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/error-boundary";
import { installDurableFlush } from "./lib/durable-flush";
import "katex/dist/katex.min.css";
import "./index.css";

// Process-lifetime listener, so it belongs here rather than in a component
// effect where StrictMode's double-invoke and unmount would churn it (#1184).
void installDurableFlush();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
