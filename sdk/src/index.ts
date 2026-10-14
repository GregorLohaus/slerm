import { apiVersion } from "./generated.js";
import type { Event, Fs, Slerm, System } from "./generated.js";
export * from "./generated.js";

export interface Plugin {
  activate?(slerm: Slerm): void | Promise<void>;
  onEvent?(event: Event, slerm: Slerm): void | Promise<void>;
}

declare const __slerm: { activate(plugin: Plugin, version: number): void; fs: Fs; system: System };

declare global {
  // Slerm installs these timers; they behave as in browsers and Node.js.
  function setTimeout<A extends unknown[]>(callback: (...args: A) => void, ms?: number, ...args: A): number;
  function setInterval<A extends unknown[]>(callback: (...args: A) => void, ms?: number, ...args: A): number;
  function clearTimeout(handle: number | undefined): void;
  function clearInterval(handle: number | undefined): void;
}

/** Filesystem access. Calls run on this plugin's worker thread and take absolute paths. */
export const fs: Fs = __slerm.fs;

/** Processes, HTTP, and environment variables. `run` and `fetch` do not block other events. */
export const system: System = __slerm.system;

/** Register one plugin in its embedded Boa runtime. Bundle this entry point. */
export function definePlugin(plugin: Plugin): void {
  __slerm.activate(plugin, apiVersion);
}
