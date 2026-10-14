/** POSIX path helpers for the absolute paths `fs` accepts and returns. */
export const path = {
  /** Join segments and resolve `.` and `..`. A later absolute segment restarts the path. */
  join(...parts: string[]): string {
    const out: string[] = [];
    let absolute = false;
    for (const part of parts) {
      if (part.startsWith("/")) { out.length = 0; absolute = true; }
      for (const segment of part.split("/")) {
        if (segment === "" || segment === ".") continue;
        if (segment === ".." && out.length && out.at(-1) !== "..") out.pop();
        else if (segment !== ".." || !absolute) out.push(segment);
      }
    }
    return (absolute ? "/" : "") + out.join("/") || (absolute ? "/" : ".");
  },
  dirname(p: string): string {
    const trimmed = p.replace(/(.)\/+$/, "$1");
    const index = trimmed.lastIndexOf("/");
    return index < 0 ? "." : index === 0 ? "/" : trimmed.slice(0, index);
  },
  basename(p: string): string {
    return p.replace(/\/+$/, "").split("/").at(-1) || (p.startsWith("/") ? "/" : "");
  },
};
