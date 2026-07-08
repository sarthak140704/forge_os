import { create } from "zustand";

interface UiState {
  selectedMissionId: string | null;
  select(id: string | null): void;
}

export const useUiStore = create<UiState>((set) => ({
  selectedMissionId: null,
  select: (id) => set(() => ({ selectedMissionId: id })),
}));
