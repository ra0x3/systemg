FROM sysg-usecase-base

COPY tests/docker/usecase/parity-lib.sh /usecase/parity-lib.sh
COPY tests/docker/usecase/parity-inspect-json/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/parity-inspect-json/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
