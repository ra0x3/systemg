FROM sysg-usecase-base

COPY tests/docker/usecase/crash-dependent-selfheal/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/crash-dependent-selfheal/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
