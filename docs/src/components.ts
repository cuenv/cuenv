/**
 * MDX globals registry — components available inside MDX without `import`.
 * Wired via `<Content components={components} />` in `[...slug].astro`.
 * Add new components here as you build (or install) them.
 */

import EnvShowcase from "./components/EnvShowcase.astro";
import { Aside } from "./components/ui/aside";
import { Card } from "./components/ui/card";
import { CardGrid } from "./components/ui/card-grid";
import { LinkButton } from "./components/ui/link-button";
import { Step, Steps } from "./components/ui/steps";
import { Tabs, TabItem } from "./components/ui/tabs";

export const components = {
  Aside,
  Card,
  CardGrid,
  EnvShowcase,
  LinkButton,
  Step,
  Steps,
  TabItem,
  Tabs,
};
