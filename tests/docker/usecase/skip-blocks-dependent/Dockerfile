FROM sysg-usecase-base

COPY tests/docker/usecase/skip-blocks-dependent/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/skip-blocks-dependent/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
