import type { ReactNode } from "react";
import { useState } from "react";
import clsx from "clsx";
import Link from "@docusaurus/Link";
import styles from "./styles.module.css";

export default function InstallationCode(): ReactNode {
  const [copiedIndex, setCopiedIndex] = useState<number | null>(null);

  const handleCopy = (text: string, index: number) => {
    navigator.clipboard.writeText(text);
    setCopiedIndex(index);
    setTimeout(() => setCopiedIndex(null), 2000);
  };

  const installCommands = [
    {
      title: "Install systemg",
      command: "curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh",
      description: "Downloads and installs the latest release"
    }
  ];

  return (
    <section className={styles.installation}>
      <div
        className="container"
        style={{ display: "flex", justifyContent: "center" }}
      >
        <div className="row">
          <div className="col col--12">
            <div>
              {installCommands.map((cmd, index) => (
                <div key={index} style={{ marginBottom: index < installCommands.length - 1 ? "2rem" : 0 }}>
                  <h3 style={{ fontSize: "1.2rem", fontWeight: 600, marginBottom: "0.5rem" }}>
                    {cmd.title}
                  </h3>
                  <div className={styles.codeBlock}>
                    <div className={styles.codeHeader}>
                      <span className={styles.codeTitle}>{cmd.description}</span>
                    </div>
                    <div className={styles.codeContent}>
                    <code className={styles.codeText}>
                      $ {cmd.command}
                    </code>
                    <button
                      className={styles.copyButton}
                      onClick={() => handleCopy(cmd.command, index)}
                      title="Copy to clipboard"
                    >
                      {copiedIndex === index ? (
                        <svg
                          width="16"
                          height="16"
                          viewBox="0 0 24 24"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        >
                          <path d="M20 6L9 17l-5-5"></path>
                        </svg>
                      ) : (
                        <svg
                          width="16"
                          height="16"
                          viewBox="0 0 24 24"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        >
                          <rect
                            x="9"
                            y="9"
                            width="13"
                            height="13"
                            rx="2"
                            ry="2"
                          ></rect>
                          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
                        </svg>
                      )}
                    </button>
                    </div>
                  </div>
                </div>
              ))}
              <div
                style={{
                  display: "flex",
                  justifyContent: "flex-end",
                  marginTop: "1rem",
                }}
              >
                <Link
                  to="/docs/examples"
                  style={{
                    padding: "0.5rem 1rem",
                    backgroundColor: "var(--ifm-color-primary)",
                    color: "white",
                    borderRadius: "0.375rem",
                    textDecoration: "none",
                    fontWeight: 600,
                    fontSize: "0.9rem",
                    transition: "background-color 0.2s ease",
                    display: "inline-block",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.backgroundColor =
                      "var(--ifm-color-primary-dark)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.backgroundColor =
                      "var(--ifm-color-primary)";
                  }}
                >
                  Examples →
                </Link>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
