import { apiVersion } from "./generated.js";
import type { Event, Slerm } from "./generated.js";
export * from "./generated.js";

export interface Plugin {
  activate?(slerm: Slerm): void | Promise<void>;
  onEvent?(event: Event, slerm: Slerm): void | Promise<void>;
}

declare const __slerm: { activate(plugin: Plugin, version: number): void };

/** Register one plugin in its embedded Boa runtime. Bundle this entry point. */
export function definePlugin(plugin: Plugin): void {
  __slerm.activate(plugin, apiVersion);
}
