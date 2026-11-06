import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { tauriBackend } from "./lib/api";

const target = document.getElementById("app");
if (!target) throw new Error("no #app element");

export default mount(App, { target, props: { backend: tauriBackend() } });
