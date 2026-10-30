import { mount } from "svelte";
import App from "./App.svelte";
import { product } from "$lib/generated/product";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/hljs.css";

document.title = product.displayName;

export default mount(App, { target: document.getElementById("app")! });
