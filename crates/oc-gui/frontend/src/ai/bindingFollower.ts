/**
 * The open conversation's project binding. Reads and rebinds may overlap with a
 * conversation switch, another workspace or a late reply; each request carries
 * a sequence number and the scope epoch it belongs to, and only the newest one
 * for the conversation on screen publishes its status.
 */
import { createSignal, type Accessor } from "solid-js";
import { followsOpenProject, sessionBinding, type SessionBinding } from "./binding";

export interface BindingFollowerDeps {
  /** `sessions_binding`, untyped at the boundary. */
  status: (id: string) => Promise<unknown>;
  /** `sessions_bind`. */
  bind: (id: string) => Promise<unknown>;
  openId: Accessor<string | null>;
  epoch: Accessor<number>;
  disposed: () => boolean;
  onWarn: (message: string) => void;
}

export function createBindingFollower(deps: BindingFollowerDeps) {
  const [binding, setBindingSignal] = createSignal<SessionBinding | null>(null);
  let request = 0;

  const publish = (sequence: number, epoch: number, id: string, value: SessionBinding | null): boolean => {
    if (deps.disposed() || epoch !== deps.epoch() || sequence !== request || deps.openId() !== id) return false;
    setBindingSignal(value);
    return true;
  };

  /** Read where a conversation is bound without rebinding it. */
  const read = async (id: string): Promise<void> => {
    const epoch = deps.epoch();
    const sequence = ++request;
    try {
      publish(sequence, epoch, id, sessionBinding(await deps.status(id)));
    } catch {
      publish(sequence, epoch, id, null);
    }
  };

  /** Move the conversation onto the open project when it points elsewhere. */
  const follow = async (id: string | null): Promise<void> => {
    if (!id) return;
    const epoch = deps.epoch();
    const sequence = ++request;
    try {
      const status = sessionBinding(await deps.status(id));
      if (!status) {
        publish(sequence, epoch, id, null);
        return;
      }
      if (followsOpenProject(status)) {
        await deps.bind(id);
        publish(sequence, epoch, id, { bound: status.open, open: status.open, matches: true });
        return;
      }
      publish(sequence, epoch, id, status);
    } catch (error) {
      deps.onWarn(`conversation not bound to the open project: ${String(error)}`);
    }
  };

  /** Forget the binding on a switch; an answer already in flight is dropped. */
  const reset = (): void => {
    request += 1;
    setBindingSignal(null);
  };

  return { binding, read, follow, reset };
}
