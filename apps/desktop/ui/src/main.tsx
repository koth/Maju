import React from "react";
import ReactDOM from "react-dom/client";
import { Workbench } from "./features/workbench/Workbench";
import "./theme.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Workbench />
  </React.StrictMode>
);
