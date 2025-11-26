import type { ReactNode } from "react";
import clsx from "clsx";
import Link from "@docusaurus/Link";
import styles from "./styles.module.css";

export default function InstallationCode(): ReactNode {
  return (
    <section className={styles.installation}>
      <div
        className="container"
        style={{ display: "flex", justifyContent: "center" }}
      >
        <div className="row">
          <div className="col col--12">
            <div>
              <div className={styles.codeBlock}>
                <div className={styles.codeHeader}>
                  <span className={styles.codeTitle}>Install systemg</span>
                </div>
                <div className={styles.codeContent}>
                  <code className={styles.codeText}>
                    $ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/
                    | sh
                  </code>
                  <button
                    className={styles.copyButton}
                    onClick={() => {
                      navigator.clipboard.writeText(
                        "curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh",
                      );
                    }}
                    title="Copy to clipboard"
                  >
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
                  </button>
                </div>
              </div>
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
