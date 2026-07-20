FROM sysg-usecase-base

COPY tests/docker/usecase/restart-ambiguous/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-ambiguous/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
