/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** When "1", the IPC layer uses the in-browser mock backend instead of Tauri. */
  readonly VITE_FF_MOCK?: string;
  /** When "1" (alongside VITE_FF_MOCK=1), streams at 300 ms/word so the Stop
   *  button can be tested under the mock. */
  readonly VITE_FF_MOCK_SLOW?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare module "@fontsource-variable/inter";
