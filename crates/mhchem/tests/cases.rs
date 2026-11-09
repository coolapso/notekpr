//! mhchemParser's own test cases (its test-esm.html, 4.2.2): each input
//! with the TeX mhchemParser gives for it. `tex` cases go through
//! `expand`, `ce` and `pu` through `to_tex`. JavaScript's string escapes are
//! resolved (`'\mu'` is `mu` there, so it is here).

use notekpr_mhchem::{ErrorKind, Kind, expand, to_tex};

const CASES: &[(&str, &str, &str)] = &[
    (
        "tex",
        r#"m_{\ce{H2O}} = \pu{1.2kg}"#,
        r#"m_{{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}} = {1.2~\mathrm{kg}}"#,
    ),
    (
        "ce",
        r#"CO2 + C -> 2 CO"#,
        r#"{\mathrm{CO}{\vphantom{A}}_{\smash[t]{2}} {}+{} \mathrm{C} {}\mathrel{\longrightarrow}{} 2\,\mathrm{CO}}"#,
    ),
    (
        "ce",
        r#"Hg^2+ ->[I-] HgI2 ->[I-] [Hg^{II}I4]^2-"#,
        r#"{\mathrm{Hg}{\vphantom{A}}^{2+} {}\mathrel{\xrightarrow{\mathrm{I}{\vphantom{A}}^{-}}}{} \mathrm{HgI}{\vphantom{A}}_{\smash[t]{2}} {}\mathrel{\xrightarrow{\mathrm{I}{\vphantom{A}}^{-}}}{} [\mathrm{Hg}{\vphantom{A}}^{\mathrm{II}}\mathrm{I}{\vphantom{A}}_{\smash[t]{4}}]{\vphantom{A}}^{2-}}"#,
    ),
    (
        "ce",
        r#"H2O"#,
        r#"{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"Sb2O3"#,
        r#"{\mathrm{Sb}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}{\vphantom{A}}_{\smash[t]{3}}}"#,
    ),
    ("ce", r#"H+"#, r#"{\mathrm{H}{\vphantom{A}}^{+}}"#),
    (
        "ce",
        r#"CrO4^2-"#,
        r#"{\mathrm{CrO}{\vphantom{A}}_{\smash[t]{4}}{\vphantom{A}}^{2-}}"#,
    ),
    (
        "ce",
        r#"[AgCl2]-"#,
        r#"{[\mathrm{AgCl}{\vphantom{A}}_{\smash[t]{2}}]{\vphantom{A}}^{-}}"#,
    ),
    ("ce", r#"Y^99+"#, r#"{\mathrm{Y}{\vphantom{A}}^{99+}}"#),
    ("ce", r#"Y^{99+}"#, r#"{\mathrm{Y}{\vphantom{A}}^{99+}}"#),
    (
        "ce",
        r#"2 H2O"#,
        r#"{2\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"2H2O"#,
        r#"{2\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"0.5 H2O"#,
        r#"{0.5\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"1/2 H2O"#,
        r#"{\mathchoice{\textstyle\frac{1}{2}}{\frac{1}{2}}{\frac{1}{2}}{\frac{1}{2}}\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"(1/2) H2O"#,
        r#"{(1/2)\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"$n$ H2O"#,
        r#"{n \,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"n H2O"#,
        r#"{n\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"nH2O"#,
        r#"{n\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"n/2 H2O"#,
        r#"{\mathchoice{\textstyle\frac{n}{2}}{\frac{n}{2}}{\frac{n}{2}}{\frac{n}{2}}\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"^{227}_{90}Th+"#,
        r#"{{\vphantom{A}}^{\hphantom{227}}_{\hphantom{90}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{227}}_{\vphantom{2}\llap{\smash[t]{90}}}\mathrm{Th}{\vphantom{A}}^{+}}"#,
    ),
    (
        "ce",
        r#"^227_90Th+"#,
        r#"{{\vphantom{A}}^{\hphantom{227}}_{\hphantom{90}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{227}}_{\vphantom{2}\llap{\smash[t]{90}}}\mathrm{Th}{\vphantom{A}}^{+}}"#,
    ),
    (
        "ce",
        r#"^{0}_{-1}n^{-}"#,
        r#"{{\vphantom{A}}^{\hphantom{0}}_{\hphantom{-1}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{0}}_{\vphantom{2}\llap{\smash[t]{-1}}}\mathrm{n}{\vphantom{A}}^{-}}"#,
    ),
    (
        "ce",
        r#"^0_-1n-"#,
        r#"{{\vphantom{A}}^{\hphantom{0}}_{\hphantom{-1}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{0}}_{\vphantom{2}\llap{\smash[t]{-1}}}\mathrm{n}{\vphantom{A}}^{-}}"#,
    ),
    (
        "ce",
        r#"H{}^3HO"#,
        r#"{\mathrm{H}\mkern2mu{\vphantom{A}}^{\hphantom{3}}_{\hphantom{}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{3}}_{\vphantom{2}\llap{\smash[t]{}}}\mathrm{HO}}"#,
    ),
    (
        "ce",
        r#"H^3HO"#,
        r#"{\mathrm{H}\mkern2mu{\vphantom{A}}^{\hphantom{3}}_{\hphantom{}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{3}}_{\vphantom{2}\llap{\smash[t]{}}}\mathrm{HO}}"#,
    ),
    (
        "ce",
        r#"A -> B"#,
        r#"{\mathrm{A} {}\mathrel{\longrightarrow}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <- B"#,
        r#"{\mathrm{A} {}\mathrel{\longleftarrow}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <-> B"#,
        r#"{\mathrm{A} {}\mathrel{\longleftrightarrow}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <--> B"#,
        r#"{\mathrm{A} {}\mathrel{\longleftrightarrows}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <=> B"#,
        r#"{\mathrm{A} {}\mathrel{\longrightleftharpoons}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <=>> B"#,
        r#"{\mathrm{A} {}\mathrel{\longRightleftharpoons}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A <<=> B"#,
        r#"{\mathrm{A} {}\mathrel{\longLeftrightharpoons}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A ->[H2O] B"#,
        r#"{\mathrm{A} {}\mathrel{\xrightarrow{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A ->[{text above}][{text below}] B"#,
        r#"{\mathrm{A} {}\mathrel{\xrightarrow[{{\text{text below}}}]{{\text{text above}}}}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"A ->[$x$][$x_i$] B"#,
        r#"{\mathrm{A} {}\mathrel{\xrightarrow[{x_i }]{x }}{} \mathrm{B}}"#,
    ),
    (
        "ce",
        r#"(NH4)2S"#,
        r#"{(\mathrm{NH}{\vphantom{A}}_{\smash[t]{4}}){\vphantom{A}}_{\smash[t]{2}}\mathrm{S}}"#,
    ),
    (
        "ce",
        r#"[\{(X2)3\}2]^3+"#,
        r#"{[\{(\mathrm{X}{\vphantom{A}}_{\smash[t]{2}}){\vphantom{A}}_{\smash[t]{3}}\}{\vphantom{A}}_{\smash[t]{2}}]{\vphantom{A}}^{3+}}"#,
    ),
    (
        "ce",
        r#"CH4 + 2 $\left( \ce{O2 + 79/21 N2} \right)$"#,
        r#"{\mathrm{CH}{\vphantom{A}}_{\smash[t]{4}} {}+{} 2\,\left(  \mathrm{O}{\vphantom{A}}_{\smash[t]{2}} {}+{} \mathchoice{\textstyle\frac{79}{21}}{\frac{79}{21}}{\frac{79}{21}}{\frac{79}{21}}\,\mathrm{N}{\vphantom{A}}_{\smash[t]{2}} \right) }"#,
    ),
    (
        "ce",
        r#"H2(aq)"#,
        r#"{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mskip2mu (\mathrm{aq})}"#,
    ),
    (
        "ce",
        r#"CO3^2-_{(aq)}"#,
        r#"{\mathrm{CO}{\vphantom{A}}_{\smash[t]{3}}{\vphantom{A}}^{2-}{\vphantom{A}}_{\smash[t]{\mskip1mu (\mathrm{aq})}}}"#,
    ),
    (
        "ce",
        r#"CO3^2-{}_{(aq)}"#,
        r#"{\mathrm{CO}{\vphantom{A}}_{\smash[t]{3}}{\vphantom{A}}^{2-}{\vphantom{A}}_{\smash[t]{\mskip1mu (\mathrm{aq})}}}"#,
    ),
    (
        "ce",
        r#"NaOH(aq,$\infty$)"#,
        r#"{\mathrm{NaOH}\mskip2mu (\mathrm{aq},\infty )}"#,
    ),
    ("ce", r#"ZnS ($c$)"#, r#"{\mathrm{ZnS}\mskip2mu (c )}"#),
    (
        "ce",
        r#"ZnS (\ca$c$)"#,
        r#"{\mathrm{ZnS}\mskip2mu ({\sim}c )}"#,
    ),
    (
        "ce",
        r#"NO_x"#,
        r#"{\mathrm{NO}{\vphantom{A}}_{\smash[t]{x }}}"#,
    ),
    ("ce", r#"Fe^n+"#, r#"{\mathrm{Fe}{\vphantom{A}}^{n +}}"#),
    (
        "ce",
        r#"x Na(NH4)HPO4 ->[Delta] (NaPO3)_x + x NH3 ^ + x H2O"#,
        r#"{x\,\mathrm{Na}(\mathrm{NH}{\vphantom{A}}_{\smash[t]{4}})\mathrm{HPO}{\vphantom{A}}_{\smash[t]{4}} {}\mathrel{\xrightarrow{\mathrm{Delta}}}{} (\mathrm{NaPO}{\vphantom{A}}_{\smash[t]{3}}){\vphantom{A}}_{\smash[t]{x }} {}+{} x\,\mathrm{NH}{\vphantom{A}}_{\smash[t]{3}} \uparrow{}  {}+{} x\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    ("ce", r#"mu-Cl"#, r#"{\mathrm{mu}{-}\mathrm{Cl}}"#),
    (
        "ce",
        r#"[Pt(\eta^2-C2H4)Cl3]-"#,
        r#"{[\mathrm{Pt}(\mathrm{\eta}{\vphantom{A}}^{2}\text{-}\mathrm{C}{\vphantom{A}}_{\smash[t]{2}}\mathrm{H}{\vphantom{A}}_{\smash[t]{4}})\mathrm{Cl}{\vphantom{A}}_{\smash[t]{3}}]{\vphantom{A}}^{-}}"#,
    ),
    ("ce", r#"\beta +"#, r#"{\mathrm{\beta }{\vphantom{A}}^{+}}"#),
    (
        "ce",
        r#"^40_18Ar + \gamma{} + \nu_e"#,
        r#"{{\vphantom{A}}^{\hphantom{40}}_{\hphantom{18}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{40}}_{\vphantom{2}\llap{\smash[t]{18}}}\mathrm{Ar} {}+{} \mathrm{\gamma{}} {}+{} \mathrm{\nu}{\vphantom{A}}_{\smash[t]{e }}}"#,
    ),
    (
        "ce",
        r#"NaOH(aq,$\infty$)"#,
        r#"{\mathrm{NaOH}\mskip2mu (\mathrm{aq},\infty )}"#,
    ),
    (
        "ce",
        r#"Fe(CN)_{$\frac{6}{2}$}"#,
        r#"{\mathrm{Fe}(\mathrm{CN}){\vphantom{A}}_{\smash[t]{\frac{6}{2} }}}"#,
    ),
    (
        "ce",
        r#"X_{$i$}^{$x$}"#,
        r#"{\mathrm{X}{\vphantom{A}}_{\smash[t]{i }}{\vphantom{A}}^{x }}"#,
    ),
    (
        "ce",
        r#"X_$i$^$x$"#,
        r#"{\mathrm{X}{\vphantom{A}}_{\smash[t]{i }}{\vphantom{A}}^{x }}"#,
    ),
    (
        "ce",
        r#"$cis${-}[PtCl2(NH3)2]"#,
        r#"{cis {\text{-}}[\mathrm{PtCl}{\vphantom{A}}_{\smash[t]{2}}(\mathrm{NH}{\vphantom{A}}_{\smash[t]{3}}){\vphantom{A}}_{\smash[t]{2}}]}"#,
    ),
    ("ce", r#"CuS($hP12$)"#, r#"{\mathrm{CuS}(hP12 )}"#),
    (
        "ce",
        r#"{Gluconic Acid} + H2O2"#,
        r#"{{\text{Gluconic Acid}} {}+{} \mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}{\vphantom{A}}_{\smash[t]{2}}}"#,
    ),
    (
        "ce",
        r#"X_{{red}}"#,
        r#"{\mathrm{X}{\vphantom{A}}_{\smash[t]{\text{red}}}}"#,
    ),
    (
        "ce",
        r#"{(+)}_589{-}[Co(en)3]Cl3"#,
        r#"{{\text{(+)}}{\vphantom{A}}_{\smash[t]{589}}{\text{-}}[\mathrm{Co}(\mathrm{en}){\vphantom{A}}_{\smash[t]{3}}]\mathrm{Cl}{\vphantom{A}}_{\smash[t]{3}}}"#,
    ),
    (
        "ce",
        r#"C6H5-CHO"#,
        r#"{\mathrm{C}{\vphantom{A}}_{\smash[t]{6}}\mathrm{H}{\vphantom{A}}_{\smash[t]{5}}{-}\mathrm{CHO}}"#,
    ),
    (
        "ce",
        r#"A-B=C#D"#,
        r#"{\mathrm{A}{-}\mathrm{B}{=}\mathrm{C}{\equiv}\mathrm{D}}"#,
    ),
    (
        "ce",
        r#"A\bond{-}B\bond{=}C\bond{#}D"#,
        r#"{\mathrm{A}{-}\mathrm{B}{=}\mathrm{C}{\equiv}\mathrm{D}}"#,
    ),
    (
        "ce",
        r#"A\bond{1}B\bond{2}C\bond{3}D"#,
        r#"{\mathrm{A}{-}\mathrm{B}{=}\mathrm{C}{\equiv}\mathrm{D}}"#,
    ),
    (
        "ce",
        r#"A\bond{~}B\bond{~-}C"#,
        r#"{\mathrm{A}{\tripledash}\mathrm{B}{\rlap{\lower.1em{-}}\raise.1em{\tripledash}}\mathrm{C}}"#,
    ),
    (
        "ce",
        r#"A\bond{~--}B\bond{~=}C\bond{-~-}D"#,
        r#"{\mathrm{A}{\rlap{\lower.2em{-}}\rlap{\raise.2em{\tripledash}}-}\mathrm{B}{\rlap{\lower.2em{-}}\rlap{\raise.2em{\tripledash}}-}\mathrm{C}{\rlap{\lower.2em{-}}\rlap{\raise.2em{-}}\tripledash}\mathrm{D}}"#,
    ),
    (
        "ce",
        r#"A\bond{...}B\bond{....}C"#,
        r#"{\mathrm{A}{{\cdot}{\cdot}{\cdot}}\mathrm{B}{{\cdot}{\cdot}{\cdot}{\cdot}}\mathrm{C}}"#,
    ),
    (
        "ce",
        r#"A\bond{->}B\bond{<-}C"#,
        r#"{\mathrm{A}{\rightarrow}\mathrm{B}{\leftarrow}\mathrm{C}}"#,
    ),
    (
        "ce",
        r#"KCr(SO4)2*12H2O"#,
        r#"{\mathrm{KCr}(\mathrm{SO}{\vphantom{A}}_{\smash[t]{4}}){\vphantom{A}}_{\smash[t]{2}}\,{\cdot}\,12\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"KCr(SO4)2.12H2O"#,
        r#"{\mathrm{KCr}(\mathrm{SO}{\vphantom{A}}_{\smash[t]{4}}){\vphantom{A}}_{\smash[t]{2}}\,{\cdot}\,12\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"KCr(SO4)2 * 12 H2O"#,
        r#"{\mathrm{KCr}(\mathrm{SO}{\vphantom{A}}_{\smash[t]{4}}){\vphantom{A}}_{\smash[t]{2}}\,{\cdot}\,12\,\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"#,
    ),
    (
        "ce",
        r#"Fe^{II}Fe^{III}2O4"#,
        r#"{\mathrm{Fe}{\vphantom{A}}^{\mathrm{II}}\mathrm{Fe}{\vphantom{A}}^{\mathrm{III}}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}{\vphantom{A}}_{\smash[t]{4}}}"#,
    ),
    (
        "ce",
        r#"OCO^{.-}"#,
        r#"{\mathrm{OCO}{\vphantom{A}}^{\mkern1mu \bullet\mkern1mu -}}"#,
    ),
    (
        "ce",
        r#"NO^{(2.)-}"#,
        r#"{\mathrm{NO}{\vphantom{A}}^{(2\mkern1mu \bullet\mkern1mu )-}}"#,
    ),
    (
        "ce",
        r#"Li^x_{Li,1-2x}Mg^._{Li,x}$V$'_{Li,x}Cl^x_{Cl}"#,
        r#"{\mathrm{Li}{\vphantom{A}}^{{\times}}_{\smash[t]{\mathrm{Li}{,}\mkern1mu 1-2x }}\mathrm{Mg}{\vphantom{A}}^{\mkern1mu \bullet\mkern1mu }_{\smash[t]{\mathrm{Li}{,}\mkern1mu x }}V {\vphantom{A}}^{\prime }_{\smash[t]{\mathrm{Li}{,}\mkern1mu x }}\mathrm{Cl}{\vphantom{A}}^{{\times}}_{\smash[t]{\mathrm{Cl}}}}"#,
    ),
    (
        "ce",
        r#"O''_{i,x}"#,
        r#"{\mathrm{O}{\vphantom{A}}^{\prime \prime }_{\smash[t]{\mathrm{i}{,}\mkern1mu x }}}"#,
    ),
    (
        "ce",
        r#"M^{..}_i"#,
        r#"{\mathrm{M}{\vphantom{A}}^{\mkern1mu \bullet\mkern1mu \mkern1mu \bullet\mkern1mu }_{\smash[t]{\mathrm{i}}}}"#,
    ),
    (
        "ce",
        r#"$V$^{4'}_{Ti}"#,
        r#"{V {\vphantom{A}}^{4\prime }_{\smash[t]{\mathrm{Ti}}}}"#,
    ),
    (
        "ce",
        r#"V_{V,1}C_{C,0.8}$V$_{C,0.2}"#,
        r#"{\mathrm{V}{\vphantom{A}}_{\smash[t]{\mathrm{V}{,}\mkern1mu 1}}\mathrm{C}{\vphantom{A}}_{\smash[t]{\mathrm{C}{,}\mkern1mu 0.8}}V {\vphantom{A}}_{\smash[t]{\mathrm{C}{,}\mkern1mu 0.2}}}"#,
    ),
    ("ce", r#"A + B"#, r#"{\mathrm{A} {}+{} \mathrm{B}}"#),
    ("ce", r#"A - B"#, r#"{\mathrm{A} {}-{} \mathrm{B}}"#),
    ("ce", r#"A = B"#, r#"{\mathrm{A} {}={} \mathrm{B}}"#),
    ("ce", r#"A \pm B"#, r#"{\mathrm{A} {}\pm{} \mathrm{B}}"#),
    (
        "ce",
        r#"SO4^2- + Ba^2+ -> BaSO4 v"#,
        r#"{\mathrm{SO}{\vphantom{A}}_{\smash[t]{4}}{\vphantom{A}}^{2-} {}+{} \mathrm{Ba}{\vphantom{A}}^{2+} {}\mathrel{\longrightarrow}{} \mathrm{BaSO}{\vphantom{A}}_{\smash[t]{4}} \downarrow{} }"#,
    ),
    (
        "ce",
        r#"A v B (v) -> B ^ B (^)"#,
        r#"{\mathrm{A} \downarrow{} ~\mathrm{B} \downarrow{}  {}\mathrel{\longrightarrow}{} \mathrm{B} \uparrow{} ~\mathrm{B} \uparrow{} }"#,
    ),
    ("ce", r#"NO^*"#, r#"{\mathrm{NO}{\vphantom{A}}^{*}}"#),
    (
        "ce",
        r#"1s^2-N"#,
        r#"{1\mathrm{s}{\vphantom{A}}^{2}\text{-}\mathrm{N}}"#,
    ),
    ("ce", r#"n-Pr"#, r#"{n \text{-}\mathrm{Pr}}"#),
    ("ce", r#"iPr"#, r#"{\mathrm{iPr}}"#),
    ("ce", r#"\ca Fe"#, r#"{{\sim}\mathrm{Fe}}"#),
    (
        "ce",
        r#"A, B, C; F"#,
        r#"{\mathrm{A}{,}\mkern6mu \mathrm{B}{,}\mkern6mu \mathrm{C}{;}\mkern6mu \mathrm{F}}"#,
    ),
    ("ce", r#"{and others}"#, r#"{{\text{and others}}}"#),
    (
        "ce",
        r#"Zn^2+  <=>[+ 2OH-][+ 2H+]  $\underset{\text{amphoteres Hydroxid}}{\ce{Zn(OH)2 v}}$  <=>[+ 2OH-][+ 2H+]  $\underset{\text{Hydroxozikat}}{\ce{[Zn(OH)4]^2-}}$"#,
        r#"{\mathrm{Zn}{\vphantom{A}}^{2+} {}\mathrel{\underset{\lower6mu{ {}+{} 2\,\mathrm{H}{\vphantom{A}}^{+}}}{\overset{ {}+{} 2\,\mathrm{OH}{\vphantom{A}}^{-}}{\longrightleftharpoons}}}{} \underset{\text{amphoteres Hydroxid}}{\ce{Zn(OH)2 v}}  {}\mathrel{\underset{\lower6mu{ {}+{} 2\,\mathrm{H}{\vphantom{A}}^{+}}}{\overset{ {}+{} 2\,\mathrm{OH}{\vphantom{A}}^{-}}{\longrightleftharpoons}}}{} \underset{\text{Hydroxozikat}}{\ce{[Zn(OH)4]^2-}} }"#,
    ),
    (
        "ce",
        r#"$K = \frac{[\ce{Hg^2+}][\ce{Hg}]}{[\ce{Hg2^2+}]}$"#,
        r#"{K = \frac{[\ce{Hg^2+}][\ce{Hg}]}{[\ce{Hg2^2+}]} }"#,
    ),
    (
        "ce",
        r#"$K = \ce{\frac{[Hg^2+][Hg]}{[Hg2^2+]}}$"#,
        r#"{K =  \frac{[\mathrm{Hg}{\vphantom{A}}^{2+}][\mathrm{Hg}]}{[\mathrm{Hg}{\vphantom{A}}_{\smash[t]{2}}{\vphantom{A}}^{2+}]}}"#,
    ),
    (
        "ce",
        r#"Hg^2+ ->[I-]  $\underset{\mathrm{red}}{\ce{HgI2}}$  ->[I-]  $\underset{\mathrm{red}}{\ce{[Hg^{II}I4]^2-}}$"#,
        r#"{\mathrm{Hg}{\vphantom{A}}^{2+} {}\mathrel{\xrightarrow{\mathrm{I}{\vphantom{A}}^{-}}}{} \underset{\mathrm{red}}{\ce{HgI2}}  {}\mathrel{\xrightarrow{\mathrm{I}{\vphantom{A}}^{-}}}{} \underset{\mathrm{red}}{\ce{[Hg^{II}I4]^2-}} }"#,
    ),
    ("pu", r#"123 kJ"#, r#"{123~\mathrm{kJ}}"#),
    ("pu", r#"123 mm2"#, r#"{123~\mathrm{mm^{2}}}"#),
    (
        "pu",
        r#"123 J s"#,
        r#"{123~\mathrm{J}\mkern3mu \mathrm{s}}"#,
    ),
    (
        "pu",
        r#"123 J*s"#,
        r#"{123~\mathrm{J}\mkern1mu{\cdot}\mkern1mu \mathrm{s}}"#,
    ),
    ("pu", r#"123 kJ/mol"#, r#"{123~\mathrm{kJ}/\mathrm{mol}}"#),
    (
        "pu",
        r#"123 kJ//mol"#,
        r#"{123~\mathchoice{\textstyle\frac{\mathrm{kJ}}{\mathrm{mol}}}{\frac{\mathrm{kJ}}{\mathrm{mol}}}{\frac{\mathrm{kJ}}{\mathrm{mol}}}{\frac{\mathrm{kJ}}{\mathrm{mol}}}}"#,
    ),
    (
        "pu",
        r#"123 kJ mol^-1"#,
        r#"{123~\mathrm{kJ}\mkern3mu \mathrm{mol^{-1}}}"#,
    ),
    (
        "pu",
        r#"123 kJ*mol-1"#,
        r#"{123~\mathrm{kJ}\mkern1mu{\cdot}\mkern1mu \mathrm{mol^{-1}}}"#,
    ),
    (
        "pu",
        r#"123 kJ.mol-1"#,
        r#"{123~\mathrm{kJ}\mkern1mu{\cdot}\mkern1mu \mathrm{mol^{-1}}}"#,
    ),
    ("pu", r#"1.2e3 kJ"#, r#"{1.2\cdot 10^{3}~\mathrm{kJ}}"#),
    ("pu", r#"1,2e3 kJ"#, r#"{1{,}2\cdot 10^{3}~\mathrm{kJ}}"#),
    ("pu", r#"1.2E3 kJ"#, r#"{1.2\times 10^{3}~\mathrm{kJ}}"#),
    ("pu", r#"1,2E3 kJ"#, r#"{1{,}2\times 10^{3}~\mathrm{kJ}}"#),
    ("pu", r#"1234"#, r#"{1234}"#),
    ("pu", r#"12345"#, r#"{12\mkern2mu 345}"#),
    ("pu", r#"1°C"#, r#"{1~\mathrm{{}^{\circ}C}}"#),
    ("pu", r#"23.4782(32) m"#, r#"{23.4782(32)~\mathrm{m}}"#),
    (
        "pu",
        r#"8.00001 \pm 0.00005 nm"#,
        r#"{8.000\mkern2mu 01 {}\pm{} 0.000\mkern2mu 05~\mathrm{nm}}"#,
    ),
    ("pu", r#".25"#, r#"{.25}"#),
    ("pu", r#"1 mol "#, r#"{1~\mathrm{mol}~}"#),
    (
        "pu",
        r#"123 l//100km"#,
        r#"{123~\mathchoice{\textstyle\frac{\mathrm{l}}{100~\mathrm{km}}}{\frac{\mathrm{l}}{100~\mathrm{km}}}{\frac{\mathrm{l}}{100~\mathrm{km}}}{\frac{\mathrm{l}}{100~\mathrm{km}}}}"#,
    ),
];

#[test]
fn all_upstream_cases() {
    let mut failures = Vec::new();
    for (kind, input, expected) in CASES {
        let actual = match *kind {
            "tex" => expand(input),
            "ce" => to_tex(input, Kind::Ce),
            "pu" => to_tex(input, Kind::Pu),
            other => panic!("unknown kind {other}"),
        };
        match actual {
            Ok(actual) if actual == *expected => {}
            Ok(actual) => failures.push(format!(
                "{kind} {input:?}\n  expected {expected}\n  actual   {actual}"
            )),
            Err(e) => failures.push(format!(
                "{kind} {input:?}\n  expected {expected}\n  error    {e}"
            )),
        }
    }
    assert_eq!(CASES.len(), 117);
    assert!(
        failures.is_empty(),
        "{} of {} cases fail:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}

#[test]
fn empty_input() {
    assert_eq!(to_tex("", Kind::Ce).unwrap(), "");
    assert_eq!(to_tex("", Kind::Pu).unwrap(), "");
    assert_eq!(expand("").unwrap(), "");
    assert_eq!(expand(r"\ce{}").unwrap(), "{}");
}

#[test]
fn escapes_leave_out_the_outer_braces() {
    assert_eq!(
        to_tex(r"A & B", Kind::Ce).unwrap(),
        r"\mathrm{A}& \mathrm{B}"
    );
    assert_eq!(
        to_tex(r"A \\ B", Kind::Ce).unwrap(),
        r"\mathrm{A}\\ \mathrm{B}"
    );
}

#[test]
fn expand_leaves_the_rest_byte_for_byte() {
    let h2o = r"{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}";
    assert_eq!(
        expand("a\r\nb − c … \\ce{H2O} % x\n\\ce {H2O} \\ce{H2O").unwrap(),
        format!("a\r\nb − c … {h2o} % x\n\\ce {{H2O}} \\ce{{H2O")
    );
    assert_eq!(
        expand(r"\ce{H2O}\pu{1 kg}").unwrap(),
        format!(r"{h2o}{{1~\mathrm{{kg}}}}")
    );
}

#[test]
fn expand_translates_what_a_translation_passes_through() {
    // \ce passes TeX in $…$ through, \ce inside a command's argument too.
    assert_eq!(
        to_tex(r"$\frac{\ce{H2}}{2}$", Kind::Ce).unwrap(),
        r"{\frac{\ce{H2}}{2} }"
    );
    assert_eq!(
        expand(r"\ce{$\frac{\ce{H2}}{2}$}").unwrap(),
        r"{\frac{{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}}}{2} }"
    );
}

#[test]
fn errors_are_mhchems() {
    let error = |input: &str| to_tex(input, Kind::Ce).unwrap_err();
    let e = error(r"A\bond{xyz}B");
    assert_eq!(e.kind(), ErrorKind::UnknownBond);
    assert_eq!(e.to_string(), "mhchem Error. Unknown bond type (xyz)");
    let e = error("$a}b$");
    assert_eq!(e.kind(), ErrorKind::UnbalancedBrace);
    assert_eq!(e.to_string(), "Extra close brace or missing open brace");
    // JavaScript's `.` doesn't match a carriage return, so nothing in text does.
    assert_eq!(error("{a\rb}").kind(), ErrorKind::Unexpected);
    assert_eq!(
        expand(r"x \ce{A\bond{xyz}B}").unwrap_err().kind(),
        ErrorKind::UnknownBond
    );
}

#[test]
fn unicode_dashes_and_ellipsis_are_read_as_ascii() {
    assert_eq!(
        to_tex("A − B", Kind::Ce).unwrap(),
        to_tex("A - B", Kind::Ce).unwrap()
    );
    assert_eq!(
        to_tex("A…B", Kind::Ce).unwrap(),
        to_tex("A...B", Kind::Ce).unwrap()
    );
    assert_eq!(
        to_tex("A\nB", Kind::Ce).unwrap(),
        to_tex("A B", Kind::Ce).unwrap()
    );
}
