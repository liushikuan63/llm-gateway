import React from "react";
import { createRoot } from "react-dom/client";
import Pet from "./Pet";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Pet />
  </React.StrictMode>
);
