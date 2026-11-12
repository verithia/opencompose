import { Show, createEffect, createSignal, on, type JSX } from "solid-js";

/**
 * A disclosure whose body mounts only while it is open. A transcript can hold
 * hundreds of tool rows, so building every body up front makes scrolling slow.
 */
export function LazyDetails(props: {
  class?: string;
  classList?: Record<string, boolean | undefined>;
  open?: boolean;
  summary: JSX.Element;
  children: JSX.Element;
}) {
  const [opened, setOpened] = createSignal(props.open === true);
  let details: HTMLDetailsElement | undefined;
  createEffect(on(() => props.open, (open) => {
    if (!details) return;
    details.open = open === true;
    setOpened(open === true);
  }));
  return (
    <details
      ref={(element) => { details = element; }}
      class={props.class}
      classList={props.classList}
      onToggle={(event) => setOpened(event.currentTarget.open)}
    >
      <summary>{props.summary}</summary>
      <Show when={opened()}>{props.children}</Show>
    </details>
  );
}
