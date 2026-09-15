#!/usr/bin/env python3
"""Generate Tsuro sample PDFs: a formatted reading sample and a signed contract."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from io import BytesIO
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
from pyhanko.pdf_utils.incremental_writer import IncrementalPdfFileWriter
from pyhanko.sign import signers
from pyhanko.sign.fields import SigFieldSpec, append_signature_field
from pyhanko.sign.signers import PdfSignatureMetadata, PdfSigner
from pyhanko.stamp import TextStampStyle
from reportlab.lib.colors import Color, HexColor, white
from reportlab.lib.enums import TA_CENTER, TA_JUSTIFY, TA_LEFT, TA_RIGHT
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import mm
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas
from reportlab.platypus import (
    CondPageBreak,
    KeepTogether,
    ListFlowable,
    ListItem,
    PageBreak,
    Paragraph,
    SimpleDocTemplate,
    Spacer,
    Table,
    TableStyle,
)

ROOT = Path(__file__).resolve().parents[1]
SAMPLES = ROOT / "public" / "samples"
FIXTURES = ROOT / "src-tauri" / "tests" / "fixtures"
KEYS = Path("/tmp/folio-sample-keys")

INK = HexColor("#1C1915")
MUTED = HexColor("#6B6459")
TEAL = HexColor("#0F6B5C")
TEAL_SOFT = HexColor("#E4F2EE")
PAPER = HexColor("#F7F3EC")
RULE = HexColor("#D9D1C3")
ACCENT_LINE = HexColor("#C45C26")

def register_font(name: str, path: str) -> None:
    """Registra a fonte só se o arquivo existir.

    As amostras de texto usam as fontes do Linux do build; onde elas não
    existem, `build_outline_sample` (fontes base-14) ainda roda.
    """
    if Path(path).exists():
        pdfmetrics.registerFont(TTFont(name, path))


register_font("Inter", "/usr/share/fonts/truetype/macos/Inter-Regular.ttf")
register_font("Inter-Bold", "/usr/share/fonts/truetype/macos/Inter-Bold.ttf")
register_font("Inter-Semi", "/usr/share/fonts/truetype/macos/Inter-SemiBold.ttf")
register_font("Tinos", "/usr/share/fonts/truetype/croscore/Tinos-Regular.ttf")
register_font("Tinos-Bold", "/usr/share/fonts/truetype/croscore/Tinos-Bold.ttf")
register_font("Tinos-Italic", "/usr/share/fonts/truetype/croscore/Tinos-Italic.ttf")
register_font("Tinos-BoldItalic", "/usr/share/fonts/truetype/croscore/Tinos-BoldItalic.ttf")


def styles() -> dict[str, ParagraphStyle]:
    return {
        "kicker": ParagraphStyle(
            "kicker",
            fontName="Inter-Semi",
            fontSize=8.5,
            leading=12,
            textColor=TEAL,
            tracking=1.4,
            alignment=TA_LEFT,
        ),
        "title": ParagraphStyle(
            "title",
            fontName="Tinos-Bold",
            fontSize=28,
            leading=34,
            textColor=INK,
            spaceAfter=8,
        ),
        "subtitle": ParagraphStyle(
            "subtitle",
            fontName="Tinos-Italic",
            fontSize=12.5,
            leading=18,
            textColor=MUTED,
            spaceAfter=18,
        ),
        "h2": ParagraphStyle(
            "h2",
            fontName="Inter-Semi",
            fontSize=12,
            leading=16,
            textColor=INK,
            spaceBefore=16,
            spaceAfter=8,
        ),
        "body": ParagraphStyle(
            "body",
            fontName="Tinos",
            fontSize=11,
            leading=16.5,
            textColor=INK,
            alignment=TA_JUSTIFY,
            spaceAfter=10,
        ),
        "caption": ParagraphStyle(
            "caption",
            fontName="Inter",
            fontSize=8,
            leading=11,
            textColor=MUTED,
        ),
        "small": ParagraphStyle(
            "small",
            fontName="Inter",
            fontSize=9,
            leading=13,
            textColor=MUTED,
        ),
        "cell": ParagraphStyle(
            "cell",
            fontName="Tinos",
            fontSize=9.5,
            leading=13,
            textColor=INK,
        ),
        "cellHead": ParagraphStyle(
            "cellHead",
            fontName="Inter-Semi",
            fontSize=8,
            leading=11,
            textColor=TEAL,
        ),
        "quote": ParagraphStyle(
            "quote",
            fontName="Tinos-Italic",
            fontSize=13,
            leading=20,
            textColor=INK,
            leftIndent=12,
            spaceBefore=8,
            spaceAfter=12,
        ),
        "sign": ParagraphStyle(
            "sign",
            fontName="Inter",
            fontSize=9,
            leading=13,
            textColor=INK,
        ),
        "center": ParagraphStyle(
            "center",
            fontName="Tinos",
            fontSize=11,
            leading=16,
            alignment=TA_CENTER,
            textColor=INK,
        ),
        "right": ParagraphStyle(
            "right",
            fontName="Inter",
            fontSize=9,
            leading=12,
            alignment=TA_RIGHT,
            textColor=MUTED,
        ),
    }


def header_footer(canvas, doc, *, mark: str, running: str) -> None:
    canvas.saveState()
    width, height = A4
    canvas.setFillColor(PAPER)
    canvas.rect(0, 0, width, height, fill=1, stroke=0)
    canvas.setFillColor(TEAL)
    canvas.rect(0, height - 8, width, 8, fill=1, stroke=0)
    canvas.setFillColor(ACCENT_LINE)
    canvas.rect(0, height - 11, 42, 3, fill=1, stroke=0)

    canvas.setFillColor(MUTED)
    canvas.setFont("Inter", 8)
    canvas.drawString(22 * mm, height - 16 * mm, mark.upper())
    canvas.drawRightString(width - 22 * mm, height - 16 * mm, running)

    canvas.setStrokeColor(RULE)
    canvas.setLineWidth(0.4)
    canvas.line(22 * mm, 16 * mm, width - 22 * mm, 16 * mm)
    canvas.setFillColor(MUTED)
    canvas.setFont("Inter", 8)
    canvas.drawString(22 * mm, 11 * mm, "Tsuro · leitor de PDF de código aberto")
    canvas.drawRightString(width - 22 * mm, 11 * mm, f"{doc.page}")
    canvas.restoreState()


def build_guide(path: Path) -> None:
    s = styles()
    buf = BytesIO()
    doc = SimpleDocTemplate(
        buf,
        pagesize=A4,
        leftMargin=22 * mm,
        rightMargin=22 * mm,
        topMargin=24 * mm,
        bottomMargin=22 * mm,
        title="Como o Tsuro lê um PDF",
        author="Tsuro",
        subject="Documento de exemplo com formatação de texto",
    )

    story = [
        Paragraph("DOCUMENTO DE EXEMPLO", s["kicker"]),
        Paragraph("Como o Tsuro lê um PDF", s["title"]),
        Paragraph(
            "Um leitor deve desaparecer. O que importa é o texto, as figuras, "
            "as notas e — quando houver — a assinatura que garante autoria.",
            s["subtitle"],
        ),
        Paragraph(
            "Este arquivo existe para testar o Tsuro: tipografia com serifa, "
            "parágrafos justificados, listas, uma tabela e um índice interno. "
            "Se a leitura estiver nítida, a seleção de texto coincidir com o "
            "que você vê e a navegação for imediata, o leitor está cumprindo "
            "o ofício.",
            s["body"],
        ),
        Paragraph("O que um leitor leve precisa acertar", s["h2"]),
        Paragraph(
            "A maior parte dos visores de PDF ou pesa demais, ou erra o básico. "
            "Fontes substitutas deformam o ritmo da linha. Camadas de texto "
            "desencontradas impedem copiar um trecho. Assinaturas digitais "
            "viram um carimbo decorativo, sem verificação. Tsuro nasce no "
            "sentido oposto: um app nativo para macOS e Windows, escrito em "
            "Rust, que abre o arquivo, desenha cada página com fidelidade e "
            "explica o estado criptográfico da assinatura — se ela existir.",
            s["body"],
        ),
        Paragraph(
            "“Leve não é pobre. Leve é o que não atrapalha o olho.”",
            s["quote"],
        ),
        Paragraph("Três frentes, um só gesto", s["h2"]),
        ListFlowable(
            [
                ListItem(
                    Paragraph(
                        "<b>Renderização fiel.</b> Cada glifo, espaço e acento "
                        "português precisa cair no lugar certo. O Tsuro usa o "
                        "motor PDF.js sobre uma casca nativa em Tauri.",
                        s["body"],
                    ),
                    leftIndent=8,
                ),
                ListItem(
                    Paragraph(
                        "<b>Leitura ágil.</b> Abrir, rolar, buscar, ajustar zoom. "
                        "Atalhos de teclado. Sem conta, sem nuvem, sem telemetria.",
                        s["body"],
                    ),
                    leftIndent=8,
                ),
                ListItem(
                    Paragraph(
                        "<b>Assinaturas digitais.</b> O Tsuro localiza o dicionário "
                        "de assinatura, lê o PKCS#7, confere o intervalo de bytes "
                        "e verifica a criptografia no motor Rust.",
                        s["body"],
                    ),
                    leftIndent=8,
                ),
            ],
            bulletType="bullet",
            start="•",
            leftIndent=12,
            bulletFontName="Inter",
            bulletFontSize=10,
            spaceBefore=0,
            spaceAfter=8,
        ),
        Paragraph("Mapa rápido da interface", s["h2"]),
    ]

    table_data = [
        [
            Paragraph("Região", s["cellHead"]),
            Paragraph("Função", s["cellHead"]),
            Paragraph("Atalho", s["cellHead"]),
        ],
        [
            Paragraph("Barra superior", s["cell"]),
            Paragraph("Abrir arquivo, zoom, ajuste à largura e busca.", s["cell"]),
            Paragraph("⌘/Ctrl O, ±, 0, F", s["cell"]),
        ],
        [
            Paragraph("Páginas", s["cell"]),
            Paragraph("Rolagem contínua, seleção de texto, anotações visíveis.", s["cell"]),
            Paragraph("↑ ↓ PgUp PgDn", s["cell"]),
        ],
        [
            Paragraph("Painel direito", s["cell"]),
            Paragraph("Assinaturas digitais: signatário, validade, cobertura.", s["cell"]),
            Paragraph("⌘/Ctrl I", s["cell"]),
        ],
        [
            Paragraph("Miniaturas", s["cell"]),
            Paragraph("Salto visual entre páginas em documentos longos.", s["cell"]),
            Paragraph("⌘/Ctrl T", s["cell"]),
        ],
    ]
    table = Table(table_data, colWidths=[38 * mm, 92 * mm, 36 * mm])
    table.setStyle(
        TableStyle(
            [
                ("BACKGROUND", (0, 0), (-1, 0), TEAL_SOFT),
                ("TEXTCOLOR", (0, 0), (-1, 0), TEAL),
                ("BACKGROUND", (0, 1), (-1, -1), white),
                ("VALIGN", (0, 0), (-1, -1), "TOP"),
                ("LEFTPADDING", (0, 0), (-1, -1), 8),
                ("RIGHTPADDING", (0, 0), (-1, -1), 8),
                ("TOPPADDING", (0, 0), (-1, -1), 7),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 7),
                ("GRID", (0, 0), (-1, -1), 0.4, RULE),
                ("BOX", (0, 0), (-1, -1), 0.6, TEAL),
            ]
        )
    )
    story.extend(
        [
            table,
            Spacer(1, 6),
            Paragraph("A tabela acima também serve de teste: bordas, hierarquia e alinhamento.", s["caption"]),
            PageBreak(),
            Paragraph("TIPOGRAFIA", s["kicker"]),
            Paragraph("Acentos, cifras e o ritmo da linha", s["title"]),
            Paragraph(
                "Português exige mais do que um visor “quase certo”. Cedilha, til, "
                "circunflexo e crase mudam sentido. Um leitor que troca “não” por "
                "“nao”, ou que quebra “ação” no meio do glifo, não serve.",
                s["body"],
            ),
            Paragraph(
                "Frase de prova: A ação não está em São Paulo — está em Niterói, "
                "onde a reunião das 14h30 custa R$ 1.247,90 à organização. "
                "O sr. João, a sra. Conceição e o dr. Antônio assinaram o termo "
                "às 9h, depois do café com pão de queijo. “Ótimo”, disse ela. "
                "“Péssimo”, murmurou ele. A cláusula 3.1.2 fala em 12% ao ano.",
                s["body"],
            ),
            Paragraph(
                "Outra linha, agora com itálico e ênfase: <i>quem lê um contrato "
                "precisa confiar no desenho da página tanto quanto no texto</i>. "
                "A camada de texto do Tsuro fica invisível sobre o canvas, alinhada "
                "ao glifo, para que copiar e buscar encontrem a palavra certa.",
                s["body"],
            ),
            Paragraph("Busca e índice", s["h2"]),
            Paragraph(
                "Use ⌘F ou Ctrl+F e procure por “assinatura”, “Niterói” ou “12%”. "
                "O Tsuro percorre o conteúdo extraído de cada página, não uma "
                "OCR improvisada. Em documentos digitais nativos, o resultado "
                "deve ser imediato e completo.",
                s["body"],
            ),
            Paragraph(
                "Este guia não traz assinatura digital. Para isso, abra o outro "
                "exemplo, <b>contrato-assinado.pdf</b>, e abra o painel de "
                "assinaturas. Lá o Tsuro deve mostrar a signatária, o motivo, "
                "a data, se o intervalo de bytes cobre o arquivo e se a "
                "criptografia PKCS#7 confere.",
                s["body"],
            ),
            Paragraph("O que Tsuro deliberadamente não é", s["h2"]),
            Paragraph(
                "Não é editor. Não é suíte corporativa. Não pede cadastro. "
                "A primeira versão lê, busca, navega e verifica assinaturas. "
                "Anotação, preenchimento de formulário e OCR ficam para quem "
                "quiser contribuir — o código é aberto.",
                s["body"],
            ),
            Spacer(1, 18),
            Paragraph("Fim do documento de exemplo · Tsuro 0.1", s["center"]),
        ]
    )

    doc.build(
        story,
        onFirstPage=lambda c, d: header_footer(c, d, mark="Tsuro", running="Guia de leitura"),
        onLaterPages=lambda c, d: header_footer(c, d, mark="Tsuro", running="Guia de leitura"),
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(buf.getvalue())
    print(f"wrote {path} ({path.stat().st_size} bytes)")


SECTION_TITLES = (
    "Identificação das partes",
    "Objeto do contrato",
    "Prazo e vigência",
    "Preços e reajuste",
    "Forma de pagamento",
    "Obrigações do contratado",
    "Obrigações do contratante",
    "Garantia e assistência técnica",
    "Sigilo e confidencialidade",
    "Propriedade intelectual",
    "Proteção de dados",
    "Responsabilidade civil",
    "Rescisão",
    "Penalidades",
    "Solução de controvérsias",
    "Disposições finais",
)

SUBSECTIONS = {
    4: ("Reajuste anual", "Revisão extraordinária"),
    7: ("Acesso às dependências", "Aprovação de entregas"),
    10: ("Cessão de direitos", "Uso do nome"),
    13: ("Rescisão imotivada", "Rescisão por inadimplemento"),
    15: ("Mediação", "Foro da comarca"),
}


def build_outline_sample(path: Path) -> None:
    """Amostra com sumário embutido (bookmarks) para a aba Sumário.

    Usa só as fontes base-14: precisa ser gerável em qualquer máquina, mesmo
    onde as fontes do `guia-folio` não existem. Cada seção ocupa uma página e
    as subseções ficam na página da seção — a árvore tem dois níveis e passa
    da altura do painel, para exercitar a rolagem da aba.
    """
    c = canvas.Canvas(str(path), pagesize=A4)
    c.setTitle("Contrato de exemplo com sumário")
    c.setAuthor("Tsuro")
    for number, title in enumerate(SECTION_TITLES, start=1):
        key = f"secao-{number}"
        c.bookmarkPage(key)
        c.addOutlineEntry(f"{number}. {title}", key, level=0)
        c.setFont("Helvetica-Bold", 18)
        c.drawString(22 * mm, 250 * mm, f"{number}. {title}")
        c.setFont("Helvetica", 11)
        c.drawString(
            22 * mm,
            240 * mm,
            "Página de exemplo do sumário do Tsuro — os títulos vêm dos "
            "bookmarks do próprio arquivo.",
        )
        subs = SUBSECTIONS.get(number, ())
        for sub_number, sub_title in enumerate(subs, start=1):
            sub_key = f"{key}-{sub_number}"
            c.bookmarkPage(sub_key)
            c.addOutlineEntry(f"{number}.{sub_number} {sub_title}", sub_key, level=1)
            c.drawString(28 * mm, (230 - 8 * sub_number) * mm, f"{number}.{sub_number} {sub_title}")
        c.showPage()
    c.save()
    print(f"wrote {path} ({path.stat().st_size} bytes)")


def build_contract(path: Path) -> None:
    s = styles()
    buf = BytesIO()
    doc = SimpleDocTemplate(
        buf,
        pagesize=A4,
        leftMargin=22 * mm,
        rightMargin=22 * mm,
        topMargin=24 * mm,
        bottomMargin=36 * mm,
        title="Contrato de licença de uso — Tsuro Exemplos",
        author="Tsuro Exemplos Ltda",
        subject="Contrato assinado digitalmente",
    )
    story = [
        Paragraph("CONTRATO Nº 2026-091", s["kicker"]),
        Paragraph("Termo de licença de uso do software Tsuro", s["title"]),
        Paragraph("Documento assinado digitalmente para demonstração do leitor.", s["subtitle"]),
        Paragraph(
            "Pelo presente instrumento, de um lado <b>Tsuro Exemplos Ltda</b>, "
            "inscrita no CNPJ sob o nº 00.000.000/0001-91, com sede em São Paulo/SP, "
            "doravante LICENCIANTE, e de outro lado a pessoa identificada no "
            "certificado digital aposto ao final, doravante LICENCIADA, têm entre "
            "si justo e contratado o seguinte.",
            s["body"],
        ),
        Paragraph("Cláusula 1 · Objeto", s["h2"]),
        Paragraph(
            "A LICENCIANTE concede à LICENCIADA licença gratuita, perpétua e "
            "não exclusiva para uso do software Tsuro, leitor de arquivos PDF "
            "para macOS e Windows, em conformidade com a licença MIT do projeto.",
            s["body"],
        ),
        Paragraph("Cláusula 2 · Ausência de garantia especial", s["h2"]),
        Paragraph(
            "O software é oferecido “como está”, sem garantia de adequação a um "
            "fim particular. A verificação de assinaturas digitais depende da "
            "integridade do arquivo, da validade temporal do certificado e do "
            "algoritmo suportado (PKCS#7 destacado, em regra RSA com SHA-256).",
            s["body"],
        ),
        Paragraph("Cláusula 3 · Privacidade", s["h2"]),
        Paragraph(
            "O Tsuro processa o PDF no dispositivo. Este exemplo não envia o "
            "arquivo a nenhum servidor. Certificados autoassinados, como o "
            "usado neste demonstrativo, não substituem uma autoridade "
            "certificadora de confiança pública.",
            s["body"],
        ),
        Paragraph("Cláusula 4 · Foro", s["h2"]),
        Paragraph(
            "Fica eleito o foro da comarca de São Paulo/SP para dirimir "
            "dúvidas oriundas deste termo, com renúncia de qualquer outro.",
            s["body"],
        ),
        Spacer(1, 10),
        Paragraph(
            f"São Paulo, {datetime.now().strftime('%d de %B de %Y')}.",
            s["body"],
        ),
        Spacer(1, 22),
        Paragraph("Campo de assinatura digital", s["h2"]),
        Paragraph(
            "A signatária abaixo assina com um certificado de demonstração "
            "(Maria Silva / Tsuro Exemplos Ltda). O Tsuro deve reconhecer "
            "o dicionário /Sig, exibir o PKCS#7 e confirmar se o documento "
            "permanece íntegro após a aposição da assinatura.",
            s["small"],
        ),
        Spacer(1, 48),
        Paragraph("Maria Silva · Diretora de Exemplos", s["sign"]),
        Paragraph("Assinatura digital · motivo: Aceite do contrato de demonstração", s["caption"]),
    ]
    doc.build(
        story,
        onFirstPage=lambda c, d: header_footer(c, d, mark="Contrato", running="Tsuro Exemplos Ltda"),
        onLaterPages=lambda c, d: header_footer(c, d, mark="Contrato", running="Tsuro Exemplos Ltda"),
    )
    path.write_bytes(buf.getvalue())
    print(f"wrote unsigned {path} ({path.stat().st_size} bytes)")


def issue_demo_cert() -> tuple[Path, Path]:
    KEYS.mkdir(parents=True, exist_ok=True)
    key_path = KEYS / "maria.key.pem"
    cert_path = KEYS / "maria.cert.pem"
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    subject = issuer = x509.Name(
        [
            x509.NameAttribute(NameOID.COUNTRY_NAME, "BR"),
            x509.NameAttribute(NameOID.STATE_OR_PROVINCE_NAME, "Sao Paulo"),
            x509.NameAttribute(NameOID.LOCALITY_NAME, "Sao Paulo"),
            x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Tsuro Exemplos Ltda"),
            x509.NameAttribute(NameOID.ORGANIZATIONAL_UNIT_NAME, "Documentos"),
            x509.NameAttribute(NameOID.COMMON_NAME, "Maria Silva"),
            x509.NameAttribute(NameOID.EMAIL_ADDRESS, "maria.silva@tsuro.example"),
        ]
    )
    now = datetime.now(timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(days=1))
        .not_valid_after(now + timedelta(days=3650))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=True,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=False,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .sign(key, hashes.SHA256())
    )
    key_path.write_bytes(
        key.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.TraditionalOpenSSL,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    return key_path, cert_path


def sign_contract(src: Path, dst: Path, key_path: Path, cert_path: Path) -> None:
    signer = signers.SimpleSigner.load(
        key_file=str(key_path),
        cert_file=str(cert_path),
        ca_chain_files=(str(cert_path),),
    )
    with src.open("rb") as inf:
        writer = IncrementalPdfFileWriter(inf)
        append_signature_field(
            writer,
            SigFieldSpec(sig_field_name="Assinatura", on_page=0, box=(72, 78, 320, 148)),
        )
        meta = PdfSignatureMetadata(
            field_name="Assinatura",
            name="Maria Silva",
            reason="Aceite do contrato de demonstração",
            location="São Paulo, Brasil",
            contact_info="maria.silva@tsuro.example",
            md_algorithm="sha256",
        )
        pdf_signer = PdfSigner(
            meta,
            signer=signer,
            stamp_style=TextStampStyle(
                stamp_text="Assinado digitalmente por\n%(signer)s\n%(ts)s",
                border_width=1,
            ),
        )
        with dst.open("wb") as outf:
            pdf_signer.sign_pdf(writer, output=outf)
    print(f"wrote signed {dst} ({dst.stat().st_size} bytes)")


def main() -> None:
    SAMPLES.mkdir(parents=True, exist_ok=True)
    FIXTURES.mkdir(parents=True, exist_ok=True)
    guide = SAMPLES / "guia-folio.pdf"
    outline = SAMPLES / "sumario-folio.pdf"
    unsigned = SAMPLES / "contrato-rascunho.pdf"
    signed = SAMPLES / "contrato-assinado.pdf"
    # Só a amostra de sumário dispensa as fontes do build: gera primeiro.
    build_outline_sample(outline)
    build_guide(guide)
    build_contract(unsigned)
    key_path, cert_path = issue_demo_cert()
    sign_contract(unsigned, signed, key_path, cert_path)
    unsigned.unlink()
    fixture = FIXTURES / "contrato-assinado.pdf"
    fixture.write_bytes(signed.read_bytes())
    (FIXTURES / "guia-folio.pdf").write_bytes(guide.read_bytes())
    print("fixtures copied")


if __name__ == "__main__":
    main()
