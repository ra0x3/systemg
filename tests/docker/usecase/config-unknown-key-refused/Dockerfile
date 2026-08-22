FROM sysg-usecase-base

COPY tests/docker/usecase/config-unknown-key-refused/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/config-unknown-key-refused/good.yaml /usecase/good.yaml
COPY tests/docker/usecase/config-unknown-key-refused/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
