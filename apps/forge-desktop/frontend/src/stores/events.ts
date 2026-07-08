import { create } from "zustand";
import type { EventEnvelope, ForgeEvent } from "@/lib/events";

interface EventsState {
  events: EventEnvelope[];
  lastSeq: number;
  /** goal_id -> mission_id (built from goal_created). */
  goalToMission: Record<string, string>;
  /** task_id -> goal_id (built from task_created). */
  taskToGoal: Record<string, string>;
  push(env: EventEnvelope): void;
  hydrate(all: EventEnvelope[]): void;
  clear(): void;
}

const MAX = 500;

function indexOne(
  ev: ForgeEvent,
  goalToMission: Record<string, string>,
  taskToGoal: Record<string, string>,
) {
  if (ev.type === "goal_created") {
    goalToMission[ev.id] = ev.mission_id;
  } else if (ev.type === "task_created") {
    taskToGoal[ev.id] = ev.goal_id;
  }
}

export const useEventsStore = create<EventsState>((set) => ({
  events: [],
  lastSeq: 0,
  goalToMission: {},
  taskToGoal: {},
  push: (env) =>
    set((s) => {
      const goalToMission = { ...s.goalToMission };
      const taskToGoal = { ...s.taskToGoal };
      indexOne(env.event, goalToMission, taskToGoal);
      const events = [env, ...s.events].slice(0, MAX);
      return {
        events,
        lastSeq: Math.max(s.lastSeq, env.seq),
        goalToMission,
        taskToGoal,
      };
    }),
  hydrate: (all) =>
    set(() => {
      const goalToMission: Record<string, string> = {};
      const taskToGoal: Record<string, string> = {};
      // Index across ALL events (oldest first) so mappings win.
      const asc = [...all].sort((a, b) => a.seq - b.seq);
      for (const env of asc) {
        indexOne(env.event, goalToMission, taskToGoal);
      }
      const sorted = [...all].sort((a, b) => b.seq - a.seq).slice(0, MAX);
      const lastSeq = sorted.length ? Math.max(...sorted.map((e) => e.seq)) : 0;
      return { events: sorted, lastSeq, goalToMission, taskToGoal };
    }),
  clear: () =>
    set(() => ({ events: [], lastSeq: 0, goalToMission: {}, taskToGoal: {} })),
}));
