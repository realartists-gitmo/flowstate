# DOCX interpreter conversion logic

- Input `.docx` bytes are cleaned before parsing.
- Unsupported parser enum values are normalized: underline -> `single`, highlight -> `yellow`, border -> `single`, justification -> `left`, tab alignment -> `left`, tab leader -> `none`, section type -> `continuous`, page orientation -> `portrait`, style type -> `paragraph`.
- DOCX paragraph styles map by style id/name: `Heading1`/`Pocket` -> Pocket, `Heading2`/`Hat` -> Hat, `Heading3`/`Block` -> Block, `Heading4`/`Tag` -> Tag, `Analytic` -> Analytic, `Undertag` -> Undertag, otherwise Normal.
- Heading-like paragraph formatting can also infer styles: outline level 0 + bold 26pt -> Pocket; level 1 + bold 22pt -> Hat; level 2 + bold underline 16pt -> Block; level 3 + bold color -> Tag.
- DOCX run styles map by style id/name: `Style13ptBold`/`Cite`/`OldCite` -> Cite, `Emphasis` -> Emphasis, `StyleUnderline`/`Underline` -> Underline.
- Heading character styles map to contextual semantics: Heading1/Pocket character -> Cite; Heading2/Hat, Heading3/Block, Heading4/Tag character -> Emphasis.
- Direct or resolved underline is treated as underline-on unless the value is `none`.
- In Tag, Analytic, and Undertag paragraphs, underline becomes direct underline and semantic underline is suppressed.
- In heading-like paragraphs, semantic underline is suppressed unless it came from an explicit non-underline semantic style.
- Direct or resolved strikethrough/double-strikethrough maps to strikethrough.
- Word highlight maps to `HighlightStyle::Spoken`.
- Word run shading maps to `HighlightStyle::Spoken`.
- Highlight or shading without an explicit run semantic can also infer semantic Underline.
- Cite inference is contextual after headings: the first non-Undertag text paragraph after a heading can promote bold text to Cite.
- Entirely bold citation paragraphs can promote all text to Cite when short, when larger-size runs exist, when highlighted/shaded runs exist, or through leading digit heuristics.
- Bold-off cancels an explicit Cite style.
- Plain normal text with explicit source size <= 8pt and no underline/highlight/shading becomes Condensed.
- Unknown paragraph and run style ids are reported but imported with fallback behavior.
