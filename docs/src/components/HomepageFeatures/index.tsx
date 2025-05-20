import type { ReactNode } from "react";
import clsx from "clsx";
import Heading from "@theme/Heading";
import styles from "./styles.module.css";

type FeatureItem = {
  title: string;
  Svg: React.ComponentType<React.ComponentProps<"svg">>;
  description: ReactNode;
};

const FeatureList: FeatureItem[] = [
  {
    title: "Easy to Use",
    Svg: require("@site/static/img/undraw_docusaurus_mountain.svg").default,
    description: (
      <>
        systemg's lightweight binary can be installed with a single simple command,
          and the binary runs a simple no-fluff process.
      </>
    ),
  },
  {
    title: "Powerful feature set",
    Svg: require("@site/static/img/undraw_docusaurus_tree.svg").default,
    description: (
      <>
        With simple commands, systemg allows users to start, stop, and monitor processes with
          features including auto-restarts for failed processes, and webhook integrations.
      </>
    ),
  },
  {
    title: "Modern features",
    Svg: require("@site/static/img/undraw_docusaurus_react.svg").default,
    description: (
      <>
        With features such webhooks, systemg is a modern process manager that lets users
          integrate into their existing ecosystems.
      </>
    ),
  },
];

function Feature({ title, Svg, description }: FeatureItem) {
  return (
    <div className={clsx("col col--4")}>
      <div className="text--center">
        <Svg className={styles.featureSvg} role="img" />
      </div>
      <div className="text--center padding-horiz--md">
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
      </div>
    </div>
  );
}

export default function HomepageFeatures(): ReactNode {
  return (
    <section className={styles.features}>
      <div className="container">
        <div className="row">
          {FeatureList.map((props, idx) => (
            <Feature key={idx} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
