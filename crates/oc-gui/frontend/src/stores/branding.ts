import mark from "../assets/logo/mark.svg?no-inline";

/** The vector mark is an alpha mask that inherits the surface text color. */
export const LOGO_MARK = mark;

/** `OpenCompose - <project>`, or just `OpenCompose` for an unnamed project. */
export function appTitle(project: string | null | undefined): string {
  const name = (project ?? "").trim();
  return name ? `OpenCompose - ${name}` : "OpenCompose";
}
