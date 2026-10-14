import { apiVersion } from "./generated.js";
import type { Event, Fs, Slerm } from "./generated.js";
export * from "./generated.js";
export { path } from "./path.js";
export { Tree, type TreeElement, type TreeEvent, type TreeSource } from "./tree.js";

export interface Plugin {
  activate?(slerm: Slerm): void | Promise<void>;
  onEvent?(event: Event, slerm: Slerm): void | Promise<void>;
}

declare const __slerm: { activate(plugin: Plugin, version: number): void; fs: Fs };

/** Filesystem access. Calls run on this plugin's worker thread and take absolute paths. */
export const fs: Fs = __slerm.fs;

/** Register one plugin in its embedded Boa runtime. Bundle this entry point. */
export function definePlugin(plugin: Plugin): void {
  __slerm.activate(plugin, apiVersion);
}
