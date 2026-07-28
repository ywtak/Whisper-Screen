/**
 * Subscribes to the backend's pipeline events and folds them into view state.
 *
 * The reducer is the frontend's *only* model of what the pipeline is doing — it
 * never polls and never infers. Rust already owns the state machine; duplicating
 * it here would just create two things that can disagree.
 */

import { useEffect, useReducer } from "react";
import { listen } from "@tauri-apps/api/event";

import { PIPELINE_EVENT, type PipelineEvent, type Timings } from "../lib/ipc";

export type Phase = "idle" | "recording" | "latched" | "finalizing";

export interface PipelineState {
  phase: Phase;
  sessionId: number | null;
  /** RMS of the last ~100 ms, 0..1. */
  level: number;
  elapsedMs: number;
  /** True when the mic had to be opened on demand — the take may be clipped. */
  coldStart: boolean;
  capture: { width: number; height: number; monitor: string; elapsedMs: number } | null;
  /** Raw transcript, set as soon as STT returns; `ready` replaces it with the cleaned text. */
  transcript: string | null;
  lastReady: {
    sessionId: number;
    durationMs: number;
    hasImage: boolean;
    truncated: boolean;
    timings: Timings;
  } | null;
  error: string | null;
}

const initialState: PipelineState = {
  phase: "idle",
  sessionId: null,
  level: 0,
  elapsedMs: 0,
  coldStart: false,
  capture: null,
  transcript: null,
  lastReady: null,
  error: null,
};

function reduce(state: PipelineState, event: PipelineEvent): PipelineState {
  switch (event.type) {
    case "armed":
      return {
        ...initialState,
        // Keep the previous result on screen so it doesn't flash away the
        // instant the next recording starts.
        lastReady: state.lastReady,
        phase: "recording",
        sessionId: event.sessionId,
        coldStart: !event.prewarmed,
      };

    case "latched":
      return { ...state, phase: "latched" };

    case "recording":
      // Guard against a stray tick from a session that has been superseded.
      if (state.sessionId !== event.sessionId) return state;
      return { ...state, level: event.level, elapsedMs: event.elapsedMs };

    case "captureDone":
      if (state.sessionId !== event.sessionId) return state;
      return {
        ...state,
        capture: {
          width: event.width,
          height: event.height,
          monitor: event.monitor,
          elapsedMs: event.elapsedMs,
        },
      };

    case "finalizing":
      if (state.sessionId !== event.sessionId) return state;
      return { ...state, phase: "finalizing", level: 0 };

    case "transcribed":
      if (state.sessionId !== event.sessionId) return state;
      return { ...state, transcript: event.text };

    case "ready":
      return {
        ...state,
        phase: "idle",
        level: 0,
        // The raw transcript shown since `transcribed` is replaced by the
        // final text — identical when cleanup was off or fell back.
        transcript: event.text,
        lastReady: {
          sessionId: event.sessionId,
          durationMs: event.durationMs,
          hasImage: event.hasImage,
          truncated: event.truncated,
          timings: event.timings,
        },
        error: null,
      };

    case "cancelled":
      return { ...initialState, lastReady: state.lastReady };

    case "failed":
      return { ...initialState, lastReady: state.lastReady, error: event.message };

    case "idle":
      // Only reset if nothing new has already started.
      return state.phase === "idle" ? state : { ...state, phase: "idle", level: 0 };
  }
}

export function usePipeline(): PipelineState {
  const [state, dispatch] = useReducer(reduce, initialState);

  useEffect(() => {
    // `listen` resolves to an unlisten function. React may unmount before the
    // promise settles, so track that and clean up either way — otherwise a hot
    // reload stacks duplicate listeners and every event is processed twice.
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<PipelineEvent>(PIPELINE_EVENT, (event) => dispatch(event.payload)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return state;
}
