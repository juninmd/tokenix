FROM gcr.io/oss-fuzz-base/base-builder-rust

COPY . $SRC/tokenix
WORKDIR $SRC/tokenix
COPY .clusterfuzzlite/build.sh $SRC/build.sh

# O compile do ClusterFuzzLite instala libFuzzingEngine.a em /usr/lib e escreve em
# $OUT, entao precisa rodar como root; com `USER app` o build falha com
# "cp: cannot create regular file '/usr/lib/libFuzzingEngine.a': Permission denied".
# Imagem efemera, so de build/fuzz, nunca publicada: DS-0002 aceito aqui.
# trivy:ignore:AVD-DS-0002
