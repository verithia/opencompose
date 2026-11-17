/**
 * The one card slot above the composer: a destructive call waiting for an
 * answer, or a question the model asked. A turn asks one thing at a time, so
 * one value owns the whole slot and every clear drops all of it.
 */
import { createSignal } from "solid-js";
import type { ApprovalRequest, UserQuestion } from "./wire";

export interface ApprovalCard extends ApprovalRequest {
  kind: "approval";
  /** The turn that asked. */
  requestId: string;
  /** The object the call touches, resolved once when the card opens. */
  target: string | null;
}

export interface QuestionCard extends UserQuestion {
  kind: "question";
  /** The turn that asked. */
  requestId: string;
  /** The draft answer, kept while the card is open. */
  replyText: string;
  /** Why the host refused the last answer; the card stays open to correct it. */
  error: string | null;
  answering: boolean;
}

export type PanelCard = ApprovalCard | QuestionCard;

export function createCardStore() {
  const [card, setCard] = createSignal<PanelCard | null>(null);
  const approval = (): ApprovalCard | null => {
    const value = card();
    return value?.kind === "approval" ? value : null;
  };
  const question = (): QuestionCard | null => {
    const value = card();
    return value?.kind === "question" ? value : null;
  };
  const updateQuestion = (change: (value: QuestionCard) => QuestionCard): void => {
    setCard((value) => value?.kind === "question" ? change(value) : value);
  };
  return {
    card,
    approval,
    question,
    answering: () => question()?.answering ?? false,
    /** A destructive call the turn is paused on. */
    showApproval(requestId: string, request: ApprovalRequest, target: string | null): void {
      setCard({ ...request, kind: "approval", requestId, target });
    },
    /** A question the model asked; a fresh card has no draft or error. */
    showQuestion(requestId: string, request: UserQuestion): void {
      setCard({ ...request, kind: "question", requestId, replyText: "", error: null, answering: false });
    },
    clear(): void {
      setCard(null);
    },
    /** Clear only the card the call names; a later card is left alone. */
    clearIf(requestId: string, callId: string): void {
      const value = card();
      if (value?.requestId === requestId && value.call_id === callId) setCard(null);
    },
    setReplyText(text: string): void {
      updateQuestion((value) => ({ ...value, replyText: text }));
    },
    setError(error: string | null): void {
      updateQuestion((value) => ({ ...value, error }));
    },
    setAnswering(answering: boolean): void {
      updateQuestion((value) => ({ ...value, answering }));
    },
  };
}
