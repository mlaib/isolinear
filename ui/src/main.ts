import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { applyTheme, savedTheme } from "./lib/colorScheme";

// Before the first paint, so a chosen theme never flashes the other one.
applyTheme(savedTheme());

const app = mount(App, { target: document.getElementById("app")! });

export default app;
