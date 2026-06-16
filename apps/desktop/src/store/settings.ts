import { create } from "zustand";
import type { SettingsSectionId } from "@/components/settings/registry";

/** A section's reset-to-defaults action, registered while that section is active. */
type ResetHandler = () => void;

interface SettingsState {
  open: boolean;
  activeSection: SettingsSectionId;
  /** Set by the active section; `null` keeps the footer "Reset to defaults" disabled. */
  resetHandler: ResetHandler | null;
  openSettings: () => void;
  closeSettings: () => void;
  toggleSettings: () => void;
  setSection: (id: SettingsSectionId) => void;
  /** Active section wires (or clears) its reset handler. */
  registerResetHandler: (handler: ResetHandler | null) => void;
}

export const useSettingsStore = create<SettingsState>((set) => ({
  open: false,
  activeSection: "appearance",
  resetHandler: null,
  openSettings: () => set({ open: true }),
  closeSettings: () => set({ open: false }),
  toggleSettings: () => set((s) => ({ open: !s.open })),
  // Switching sections drops the previous section's reset handler; the newly
  // active section re-registers its own (if any) on mount.
  setSection: (id) => set({ activeSection: id, resetHandler: null }),
  registerResetHandler: (handler) => set({ resetHandler: handler }),
}));
