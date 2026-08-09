// What matters about an icon *family* is that it stays one. These test the
// properties that make it a set rather than a pile: one grid, one weight, one
// colour source, and no glyph announcing itself to a screen reader beside a
// label that already says the same word.

import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { KIND_ICONS, KindIcon } from "./icons";

function svgFor(kind: string): SVGSVGElement {
  const { container } = render(<KindIcon kind={kind} />);
  const svg = container.querySelector("svg");
  if (!svg) throw new Error(`no svg for ${kind}`);
  return svg as SVGSVGElement;
}

describe("the icon family", () => {
  /// The thing that makes a set look drawn by one hand. A glyph on a different
  /// grid or at a different weight reads as clip art next to the others.
  it("draws every glyph on one grid at one weight", () => {
    for (const kind of Object.keys(KIND_ICONS)) {
      const svg = svgFor(kind);
      expect(svg.getAttribute("viewBox"), kind).toBe("0 0 16 16");
      expect(svg.getAttribute("stroke-width"), kind).toBe("1.5");
      expect(svg.getAttribute("stroke-linecap"), kind).toBe("round");
    }
  });

  /// Colour comes from the surrounding text, so a glyph needs no theme of its
  /// own and cannot go stale when one changes.
  it("takes its colour from the text around it", () => {
    for (const kind of Object.keys(KIND_ICONS)) {
      const svg = svgFor(kind);
      expect(svg.getAttribute("stroke"), kind).toBe("currentColor");
      expect(svg.getAttribute("fill"), kind).toBe("none");
    }
  });

  /// Every use sits beside its own label, so announcing the icon too would
  /// read the same word twice.
  it("is hidden from assistive technology", () => {
    for (const kind of Object.keys(KIND_ICONS)) {
      expect(svgFor(kind).getAttribute("aria-hidden"), kind).toBe("true");
    }
  });

  /// A wrong-but-present icon is read as meaning something; a missing one is
  /// read as nothing, which is the truth when a kind has no glyph.
  it("renders nothing for a kind it does not know", () => {
    const { container } = render(<KindIcon kind="Wormhole" />);
    expect(container.querySelector("svg")).toBeNull();
  });

  /// The nav and the palette both look kinds up here rather than each picking
  /// their own, which is the only reason they cannot disagree.
  it("covers every kind the console shows", () => {
    for (const kind of [
      "Host",
      "VM",
      "Network",
      "Security group",
      "Volume",
      "Storage pool",
      "Image",
      "Template",
      "Task",
      "Event",
      "Project",
      "Access control",
      "Settings",
      "Overview",
      "Action",
      "Page",
    ]) {
      expect(KIND_ICONS[kind], kind).toBeDefined();
    }
  });
});
