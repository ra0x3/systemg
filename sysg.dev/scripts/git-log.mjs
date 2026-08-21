import { execFileSync } from "node:child_process";

const SEP = String.fromCharCode(31);

export const TAGS = execFileSync("git", ["tag", "--sort=creatordate"], { encoding: "utf8" })
  .split("\n")
  .filter(Boolean);

export function commitsFor(tag) {
  const i = TAGS.indexOf(tag);
  if (i < 0) return [];
  const range = i === 0 ? tag : `${TAGS[i - 1]}..${tag}`;
  let out;
  try {
    out = execFileSync("git", ["log", "--no-merges", "--pretty=format:%h%x1f%s", range], { encoding: "utf8" });
  } catch {
    return [];
  }
  return out
    .split("\n")
    .filter(Boolean)
    .map((line) => {
      const [sha, subject] = line.split(SEP);
      return { sha, subject };
    })
    .filter((c) => c.subject && !/^release: /.test(c.subject));
}

export function changelog(tag, body) {
  const commits = commitsFor(tag);
  if (!commits.length) return body;
  const lines = commits.map((c) => `- ${c.subject} (\`${c.sha}\`)`);
  const link = /Full Changelog\*{0,2}:?\s*(\S+)/.exec(body || "");
  const tail = link ? `\n\n[Full changelog on GitHub](${link[1]})` : "";
  return `## Changes\n\n${lines.join("\n")}${tail}`;
}
