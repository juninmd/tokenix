FROM gcr.io/oss-fuzz-base/base-builder-rust

COPY . $SRC/tokenix
WORKDIR $SRC/tokenix
COPY .clusterfuzzlite/build.sh $SRC/build.sh

# executa app sem privilegio (DS-0002)
RUN groupadd -g 10001 app && useradd -u 10001 -m -g app -s /bin/sh app
USER app
