import type { ReactNode } from "react";
import clsx from "clsx";
import styles from "./styles.module.css";

export default function InstallationCode(): ReactNode {
  return (
    <section className={styles.installation}>
      <div className="container">
        <div className="row">
          <div className="col col--12">
            <div className={styles.codeBlock}>
              <div className={styles.codeHeader}>
                <span className={styles.codeTitle}>Install systemg</span>
              </div>
              <div className={styles.codeContent}>
                <code className={styles.codeText}>
                  $ curl -fsSL https://sh.sysg.dev | sh
                </code>
                <button
                  className={styles.copyButton}
                  onClick={() => {
                    navigator.clipboard.writeText(
                      "curl -fsSL https://sh.sysg.dev | sh",
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
          </div>
        </div>
      </div>
    </section>
  );
}
